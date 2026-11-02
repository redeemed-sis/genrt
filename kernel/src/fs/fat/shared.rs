//! Logical physical-disk reservation shared by independent partition volumes.

use crate::block::BlockDevice;
use crate::{
    config::KERNEL_THREAD_CAPACITY,
    errno::{self, Errno},
    sched::{self, WaitCause, WaitToken},
    sync::SpinLock,
};
use alloc::boxed::Box;
use core::task::Poll;

struct State {
    device: Option<Box<dyn BlockDevice>>,
    waiters: [Option<WaitToken>; KERNEL_THREAD_CAPACITY],
}

/// Transfer-only physical owner; no lock is retained over device operations.
pub(crate) struct SharedDisk {
    state: SpinLock<State>,
    capacity: u64,
    readonly: bool,
}

/// Detached exact notifications, completed only after all owners are published.
pub(crate) struct DiskWaiters([Option<WaitToken>; KERNEL_THREAD_CAPACITY]);

impl DiskWaiters {
    /// Notify claimed waiters outside owner locks, without allocation or I/O.
    /// # Returns
    /// Nothing; stale or already-completed exact tokens are harmless.
    pub(crate) fn wake(self) {
        for token in self.0.into_iter().flatten() {
            let _ = sched::complete_wait(token, WaitCause::Notified);
        }
    }
}

impl SharedDisk {
    /// Detach a test wait without invoking the uninitialized host scheduler.
    /// # Arguments
    /// * `index` - Bounded thread slot whose exact registration is inspected.
    /// # Returns
    /// Registered token or none, without allocation, I/O or blocking.
    /// # Panics
    /// Panics if `index` exceeds kernel thread capacity.
    #[cfg(test)]
    pub(super) fn take_waiter(&self, index: usize) -> Option<WaitToken> {
        self.state.lock().waiters[index].take()
    }

    /// Cache stable device properties and stage its exclusive owner.
    /// Ordinary-task only; no allocation, I/O, or IRQ changes.
    /// # Arguments
    /// * `device` - Physical driver owner, never concurrently accessible.
    /// # Returns
    /// Available reservation with bounded inline waiter storage.
    pub(crate) fn new(device: Box<dyn BlockDevice>) -> Self {
        Self {
            capacity: device.capacity_sectors(),
            readonly: device.is_read_only(),
            state: SpinLock::new(State {
                device: Some(device),
                waiters: [None; KERNEL_THREAD_CAPACITY],
            }),
        }
    }

    /// Query cached sector capacity even while another volume owns the device.
    /// # Returns
    /// Stable capacity, without locking, allocation, blocking, or I/O.
    pub(crate) fn capacity_sectors(&self) -> u64 {
        self.capacity
    }

    /// Query cached physical write protection while the device is detached.
    /// # Returns
    /// Stable media policy, without locking, allocation, blocking, or I/O.
    pub(crate) fn is_read_only(&self) -> bool {
        self.readonly
    }

    /// Attempt ownership before any filesystem operation, never sleeping.
    /// # Arguments
    /// * `wake` - Exact prepared wait to register atomically on contention.
    /// # Returns
    /// Exclusive device, or `Pending` with a registered token. The short lock
    /// only transfers ownership; no allocation, destruction, or I/O occurs.
    /// # Errors
    /// `EAGAIN` if busy and `wake` is absent (exclusive bootstrap callers).
    pub(crate) fn acquire(
        &self,
        wake: Option<WaitToken>,
    ) -> Poll<Result<Box<dyn BlockDevice>, Errno>> {
        let mut state = self.state.lock();
        if let Some(device) = state.device.take() {
            return Poll::Ready(Ok(device));
        }
        let Some(token) = wake else {
            return Poll::Ready(Err(errno::EAGAIN));
        };
        state.waiters[token.thread().index()] = Some(token);
        Poll::Pending
    }

    /// Restore physical ownership without notifying yet; no I/O or allocation.
    /// # Arguments
    /// * `device` - Exact owner previously acquired from this reservation.
    /// # Returns
    /// Claimed waiters; publish volume availability before calling `wake`.
    /// # Panics
    /// Panics if an owner is already present, violating exclusive transfer.
    pub(crate) fn restore(&self, device: Box<dyn BlockDevice>) -> DiskWaiters {
        let mut state = self.state.lock();
        assert!(state.device.is_none());
        state.device = Some(device);
        DiskWaiters(core::mem::replace(
            &mut state.waiters,
            [None; KERNEL_THREAD_CAPACITY],
        ))
    }
}
