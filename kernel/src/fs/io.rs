//! Path-independent endpoints and common descriptor I/O. No callback may sleep.

use super::{
    backend::{IO_CAPACITY, Operation, Reply, Request},
    fd::{OpenDescription, Transaction},
};
use crate::{
    config::KERNEL_THREAD_CAPACITY,
    errno::{self, Errno},
    sched::{self, WaitCause, WaitToken},
    sync::SpinLock,
};
use alloc::{boxed::Box, sync::Arc};
use core::task::Poll;

/// Shared descriptor target. The outer Arc keeps every table slot one word.
pub(crate) enum Descriptor {
    /// VFS ownership and offset/transaction semantics remain unchanged.
    Vfs(Arc<OpenDescription>),
    /// Endpoint with no namespace node, mount, or Location.
    Endpoint(Box<Endpoint>),
}

/// Non-VFS I/O contract. Callbacks run in finite IRQ-enabled task windows,
/// outside process/scheduler/endpoint locks. State and staging are prepared at
/// construction, never allocated per small read/write. Pending effects belong
/// to the endpoint/request and must not be submitted twice on restart.
pub(crate) trait IoObject: Send + Sync {
    /// Submit/resume read or write using exclusively leased owned staging.
    /// # Arguments
    /// * `request` - Retained read/write operation and reusable staging/state.
    /// * `wake` - Prepared exact token to publish before returning Pending.
    /// # Returns
    /// Ready with an Io reply or Pending after arranging notification. A fresh
    /// token can be supplied on each resume. No scheduler handoff is allowed.
    /// # Errors
    /// Endpoint errno; without `wake`, unavailable I/O must return an error,
    /// never Pending. Report accepted writes exactly, including partial writes.
    fn poll(&self, request: &mut Request, wake: Option<WaitToken>) -> Poll<Result<Reply, Errno>>;

    /// Clear completed/abandoned progress and detach any exact registration.
    /// # Arguments
    /// * `request` - Exclusive reusable request, including pending private state.
    /// * `wait` - Last token supplied to poll, if any; cancellation must match
    ///   exactly so it cannot remove another operation's registration.
    /// # Returns
    /// Nothing. No blocking, allocation, or loss of reusable resources.
    fn reset(&self, request: &mut Request, wait: Option<WaitToken>);

    /// Bound a single partial transfer to the endpoint's natural capacity.
    /// # Arguments
    /// * `read` - True for reads, false for writes.
    /// * `requested` - Caller count, already representable as a syscall result.
    /// # Returns
    /// At most requested bytes. No side effects, allocation, or blocking.
    fn transfer_len(&self, read: bool, requested: usize) -> usize;
}

struct EndpointState {
    available: Option<Box<Request>>,
    waiters: [Option<WaitToken>; KERNEL_THREAD_CAPACITY],
}

/// Shared access policy, object and reusable request for a non-VFS endpoint.
pub(crate) struct Endpoint {
    object: Arc<dyn IoObject>,
    read: bool,
    write: bool,
    state: SpinLock<EndpointState>,
}

impl Endpoint {
    /// Stage an endpoint before installing it under process ownership.
    /// # Arguments
    /// * `object` - Shared endpoint implementation without a VFS identity.
    /// * `read` - Read permission, checked even for zero-byte I/O.
    /// * `write` - Write permission, checked even for zero-byte I/O.
    /// * `request` - Prepared private state; staging is reserved here if needed.
    /// # Returns
    /// Shared descriptor with reusable storage. Allocates only in task context.
    /// # Errors
    /// `ENOMEM` when baseline staging cannot be reserved.
    pub(crate) fn descriptor(
        object: Arc<dyn IoObject>,
        read: bool,
        write: bool,
        mut request: Request,
    ) -> Result<Arc<Descriptor>, Errno> {
        if request.buffer.capacity() < IO_CAPACITY {
            request
                .buffer
                .try_reserve_exact(IO_CAPACITY - request.buffer.len())
                .map_err(|_| errno::ENOMEM)?;
        }
        Ok(Arc::new(Descriptor::Endpoint(Box::new(Self {
            object,
            read,
            write,
            state: SpinLock::new(EndpointState {
                available: Some(Box::new(request)),
                waiters: [None; KERNEL_THREAD_CAPACITY],
            }),
        }))))
    }
}

/// Small common I/O intent; VFS retains its existing Transaction unchanged.
pub(crate) enum IoTransaction {
    Vfs(Transaction),
    Endpoint(EndpointTransaction),
}

/// Exclusive endpoint request lease, retained across exact scheduler waits.
pub(crate) struct EndpointTransaction {
    descriptor: Arc<Descriptor>,
    operation: Option<Operation>,
    length: usize,
    lease: Option<Box<Request>>,
    copied: bool,
    wait: Option<WaitToken>,
}

impl IoTransaction {
    /// Validate descriptor type/access before even zero-length operations.
    /// # Arguments
    /// * `descriptor` - Shared VFS description or path-independent endpoint.
    /// * `operation` - I/O operation, directory enumeration or synchronization.
    /// * `length` - Requested staging count.
    /// # Returns
    /// Unsubmitted intent, without allocation, owner locks, or blocking. The
    /// endpoint's pure transfer-length callback selects natural partial I/O.
    /// # Errors
    /// VFS validation errors; endpoints return `EBADF` for wrong access,
    /// `ENOTDIR` for readdir and `EINVAL` for sync/other unsupported operations.
    pub(crate) fn new(
        descriptor: Arc<Descriptor>,
        operation: Operation,
        length: usize,
    ) -> Result<Self, Errno> {
        match descriptor.as_ref() {
            Descriptor::Vfs(description) => Ok(Self::Vfs(Transaction::new(
                description.clone(),
                operation,
                length,
            )?)),
            Descriptor::Endpoint(endpoint) => {
                let read = match operation {
                    Operation::Read { .. } if endpoint.read => true,
                    Operation::Write { .. } if endpoint.write => false,
                    Operation::Read { .. } | Operation::Write { .. } => return Err(errno::EBADF),
                    Operation::Readdir { .. } => return Err(errno::ENOTDIR),
                    _ => return Err(errno::EINVAL),
                };
                let length = endpoint.object.transfer_len(read, length).min(length);
                Ok(Self::Endpoint(EndpointTransaction {
                    descriptor,
                    operation: Some(operation),
                    length,
                    lease: None,
                    copied: false,
                    wait: None,
                }))
            }
        }
    }

    /// Acquire reusable storage and copy write input once, outside owner locks.
    /// # Arguments
    /// * `wake` - Prepared exact waiter for a busy shared description.
    /// * `input` - Input copy invoked once after lease acquisition.
    /// # Returns
    /// Ready after preparation, or Pending with exact registration published.
    /// # Errors
    /// Copy/growth errors; `EAGAIN` for a busy synchronous operation.
    pub(crate) fn prepare(
        &mut self,
        wake: Option<WaitToken>,
        input: impl FnOnce(&mut [u8]) -> Result<(), Errno>,
    ) -> Poll<Result<(), Errno>> {
        match self {
            Self::Vfs(tx) => tx.prepare(wake, input),
            Self::Endpoint(tx) => tx.prepare(wake, input),
        }
    }

    /// Report output that must be validated before a consuming endpoint read.
    /// # Returns
    /// Natural partial read length, or none for VFS/write. No allocation/locks.
    pub(crate) fn consuming_read_len(&self) -> Option<usize> {
        match self {
            Self::Endpoint(tx) if matches!(tx.operation, Some(Operation::Read { .. })) => {
                Some(tx.length)
            }
            // Operation moves into the lease during preparation.
            Self::Endpoint(tx)
                if tx
                    .lease
                    .as_ref()
                    .is_some_and(|r| matches!(r.operation, Operation::Read { .. })) =>
            {
                Some(tx.length)
            }
            _ => None,
        }
    }

    /// Submit/resume one finite operation outside locks, never handing off.
    /// # Arguments
    /// * `wake` - Prepared exact token for Pending completion.
    /// # Returns
    /// Completion or Pending with owned effects retained.
    /// # Errors
    /// Endpoint/VFS errno or `EIO` for an invalid endpoint reply/count.
    pub(crate) fn poll(&mut self, wake: Option<WaitToken>) -> Poll<Result<Reply, Errno>> {
        match self {
            Self::Vfs(tx) => tx.poll(wake),
            Self::Endpoint(tx) => {
                if tx.length == 0 {
                    return Poll::Ready(Ok(Reply::Io { count: 0, next: 0 }));
                }
                match tx.prepare(wake, |_| Err(errno::EINVAL)) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => result?,
                }
                tx.wait = wake;
                let Descriptor::Endpoint(endpoint) = tx.descriptor.as_ref() else {
                    unreachable!()
                };
                let request = tx.lease.as_mut().unwrap();
                match endpoint.object.poll(request, wake) {
                    Poll::Ready(Ok(Reply::Io { count, next })) if count <= request.buffer.len() => {
                        Poll::Ready(Ok(Reply::Io { count, next }))
                    }
                    Poll::Ready(Ok(_)) => Poll::Ready(Err(errno::EIO)),
                    other => other,
                }
            }
        }
    }

    /// Borrow completed staging without locks, allocation or blocking.
    /// # Returns
    /// Reusable bytes, or empty for zero I/O without a lease.
    pub(crate) fn buffer(&self) -> &[u8] {
        match self {
            Self::Vfs(tx) => tx.buffer(),
            Self::Endpoint(tx) => tx.lease.as_ref().map_or(&[], |r| &r.buffer),
        }
    }

    /// Borrow directory encoding staging; endpoints never reach this operation.
    /// # Returns
    /// Mutable VFS staging without allocation or locks.
    /// # Panics
    /// Panics for an endpoint or unprepared VFS transaction.
    pub(crate) fn output_buffer(&mut self) -> &mut [u8] {
        match self {
            Self::Vfs(tx) => tx.output_buffer(),
            Self::Endpoint(_) => unreachable!(),
        }
    }

    /// Commit VFS position after successful copy; endpoints own their effects.
    /// # Returns
    /// Nothing; no callbacks, allocation, locks or blocking.
    pub(crate) fn commit(&mut self) {
        if let Self::Vfs(tx) = self {
            tx.commit();
        }
    }

    /// Inspect the staged VFS cursor for directory encoding.
    /// # Returns
    /// Next offset, or none for endpoints/operations without position changes.
    pub(crate) fn next_offset(&self) -> Option<u64> {
        match self {
            Self::Vfs(tx) => tx.next_offset(),
            Self::Endpoint(_) => None,
        }
    }
}

impl EndpointTransaction {
    fn prepare(
        &mut self,
        wake: Option<WaitToken>,
        input: impl FnOnce(&mut [u8]) -> Result<(), Errno>,
    ) -> Poll<Result<(), Errno>> {
        if self.length == 0 {
            return Poll::Ready(Ok(()));
        }
        let Descriptor::Endpoint(endpoint) = self.descriptor.as_ref() else {
            unreachable!()
        };
        if self.lease.is_none() {
            self.lease = {
                let mut state = endpoint.state.lock();
                match state.available.take() {
                    Some(request) => Some(request),
                    None => {
                        let Some(token) = wake else {
                            return Poll::Ready(Err(errno::EAGAIN));
                        };
                        self.wait = Some(token);
                        state.waiters[token.thread().index()] = Some(token);
                        None
                    }
                }
            };
            if self.lease.is_none() {
                return Poll::Pending;
            }
        }
        let request = self.lease.as_mut().unwrap();
        if let Some(mut operation) = self.operation.take() {
            if self.length > request.buffer.capacity() {
                request
                    .buffer
                    .try_reserve_exact(self.length - request.buffer.len())
                    .map_err(|_| errno::ENOMEM)?;
            }
            request.buffer.resize(self.length, 0);
            if let Operation::Read { limit, .. } = &mut operation {
                *limit = self.length;
            }
            request.operation = operation;
        }
        if matches!(request.operation, Operation::Write { .. }) && !self.copied {
            input(&mut request.buffer)?;
            self.copied = true;
        }
        Poll::Ready(Ok(()))
    }
}

impl Drop for EndpointTransaction {
    fn drop(&mut self) {
        let Descriptor::Endpoint(endpoint) = self.descriptor.as_ref() else {
            unreachable!()
        };
        let Some(mut request) = self.lease.take() else {
            if let Some(token) = self.wait {
                let mut state = endpoint.state.lock();
                if state.waiters[token.thread().index()] == Some(token) {
                    state.waiters[token.thread().index()] = None;
                }
            }
            return;
        };
        endpoint.object.reset(&mut request, self.wait);
        request.operation = Operation::Sync;
        let waiters = {
            let mut state = endpoint.state.lock();
            assert!(state.available.is_none());
            state.available = Some(request);
            core::mem::replace(&mut state.waiters, [None; KERNEL_THREAD_CAPACITY])
        };
        for token in waiters.into_iter().flatten() {
            let _ = sched::complete_wait(token, WaitCause::Notified);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fd::{FdTable, MAX_FDS};
    use super::*;
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Progress {
        submitted: bool,
    }

    struct Probe {
        ready: AtomicBool,
        submissions: AtomicUsize,
        resets: AtomicUsize,
        wait: SpinLock<Option<WaitToken>>,
        drops: Arc<AtomicUsize>,
    }

    impl IoObject for Probe {
        fn poll(
            &self,
            request: &mut Request,
            wake: Option<WaitToken>,
        ) -> Poll<Result<Reply, Errno>> {
            let progress = request
                .open
                .as_mut()
                .unwrap()
                .downcast_mut::<Progress>()
                .unwrap();
            if !progress.submitted {
                self.submissions.fetch_add(1, Ordering::Relaxed);
                progress.submitted = true;
            }
            if !self.ready.load(Ordering::Acquire) {
                *self.wait.lock() = wake;
                return if wake.is_some() {
                    Poll::Pending
                } else {
                    Poll::Ready(Err(errno::EAGAIN))
                };
            }
            if matches!(request.operation, Operation::Read { .. }) {
                request.buffer.fill(b'x');
            }
            Poll::Ready(Ok(Reply::Io {
                count: request.buffer.len(),
                next: 0,
            }))
        }

        fn reset(&self, request: &mut Request, wait: Option<WaitToken>) {
            self.resets.fetch_add(1, Ordering::Relaxed);
            request
                .open
                .as_mut()
                .unwrap()
                .downcast_mut::<Progress>()
                .unwrap()
                .submitted = false;
            let mut registered = self.wait.lock();
            if *registered == wait {
                *registered = None;
            }
        }

        fn transfer_len(&self, _read: bool, requested: usize) -> usize {
            requested
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn fixture(read: bool, write: bool) -> (Arc<Descriptor>, Arc<Probe>, Arc<AtomicUsize>) {
        let drops = Arc::new(AtomicUsize::new(0));
        let object = Arc::new(Probe {
            ready: AtomicBool::new(false),
            submissions: AtomicUsize::new(0),
            resets: AtomicUsize::new(0),
            wait: SpinLock::new(None),
            drops: drops.clone(),
        });
        let mut request = Request::new(Operation::Sync);
        request.open = Some(Box::new(Progress { submitted: false }));
        let descriptor = Endpoint::descriptor(object.clone(), read, write, request).unwrap();
        (descriptor, object, drops)
    }

    fn token(index: usize, sequence: u64) -> WaitToken {
        WaitToken::for_profile_test(
            sched::ThreadId::new(index, 1),
            crate::cpu::CpuId::from_index(0).unwrap(),
            sequence,
        )
    }

    fn read(descriptor: Arc<Descriptor>, length: usize) -> IoTransaction {
        IoTransaction::new(
            descriptor,
            Operation::Read {
                offset: 0,
                limit: length,
            },
            length,
        )
        .unwrap()
    }

    fn write(descriptor: Arc<Descriptor>, length: usize) -> IoTransaction {
        IoTransaction::new(
            descriptor,
            Operation::Write {
                offset: 0,
                append: false,
            },
            length,
        )
        .unwrap()
    }

    #[test]
    fn dup_lowest_full_invalid_and_atomic_replacement_detach_ownership() {
        let (source, object, _) = fixture(true, true);
        let (replaced, old_object, drops) = fixture(false, true);
        drop(old_object);
        let mut table = FdTable::new();
        assert_eq!(table.install(&mut Some(source.clone())), Ok(0));
        assert_eq!(table.install(&mut Some(replaced)), Ok(1));
        assert!(table.dup2(MAX_FDS, 1).is_err());
        assert!(table.dup2(0, MAX_FDS).is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        let removed = table.dup2(0, 1).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 0); // Caller owns unlocked cleanup.
        drop(removed);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(Arc::ptr_eq(&table.get(0).unwrap(), &table.get(1).unwrap()));
        assert_eq!(table.dup(1), Ok(2));
        drop(table.take(0).unwrap());
        assert!(matches!(table.dup2(0, 0), Err(errno::EBADF)));
        assert!(table.get(1).is_ok());
        assert_eq!(table.dup(1), Ok(0));
        for expected in 3..MAX_FDS {
            assert_eq!(table.dup(0), Ok(expected));
        }
        assert_eq!(table.dup(MAX_FDS), Err(errno::EBADF));
        assert_eq!(table.dup(0), Err(errno::EMFILE));
        assert!(table.dup2(0, 0).unwrap().is_none());
        assert!(table.dup2(0, MAX_FDS - 1).unwrap().is_some());
        let mut child = table.clone();
        drop(child.take(1).unwrap());
        assert!(table.get(1).is_ok());
        assert_eq!(child.install(&mut Some(source.clone())), Ok(1));
        object.ready.store(true, Ordering::Release);
        let (_, allocations) = crate::test_arch_stubs::allocations(|| {
            let mut tx = read(child.get(1).unwrap(), 4);
            assert!(matches!(
                tx.poll(None),
                Poll::Ready(Ok(Reply::Io { count: 4, .. }))
            ));
            assert_eq!(tx.buffer(), b"xxxx");
        });
        assert_eq!(allocations, (0, 0, 0));
        assert_eq!(
            core::mem::size_of::<FdTable>(),
            MAX_FDS * core::mem::size_of::<usize>()
        );
    }

    #[test]
    fn endpoint_pending_reuses_input_state_and_exact_cancellation_without_allocation() {
        let (descriptor, object, _) = fixture(true, true);
        let first = token(0, 1);
        let second = token(1, 2);
        let (_, allocations) = crate::test_arch_stubs::allocations(|| {
            let mut tx = write(descriptor.clone(), 4);
            assert!(matches!(
                tx.prepare(Some(first), |buffer| {
                    buffer.copy_from_slice(b"once");
                    Ok(())
                }),
                Poll::Ready(Ok(()))
            ));
            let identity = tx.buffer().as_ptr();
            assert!(tx.poll(Some(first)).is_pending());
            assert!(matches!(
                tx.prepare(Some(first), |_| panic!("recopied")),
                Poll::Ready(Ok(()))
            ));
            assert!(tx.poll(Some(first)).is_pending());
            assert_eq!(object.submissions.load(Ordering::Relaxed), 1);
            assert_eq!(*object.wait.lock(), Some(first));
            let mut busy = read(descriptor.clone(), 4);
            assert_eq!(busy.consuming_read_len(), Some(4));
            assert!(
                busy.prepare(Some(second), |_| panic!("busy copy"))
                    .is_pending()
            );
            drop(busy); // Abandoned lease waiter must not survive and wake a reused slot.
            let Descriptor::Endpoint(endpoint) = descriptor.as_ref() else {
                unreachable!()
            };
            assert_eq!(endpoint.state.lock().waiters[second.thread().index()], None);
            let mut zero = write(descriptor.clone(), 0);
            assert!(matches!(
                zero.poll(Some(second)),
                Poll::Ready(Ok(Reply::Io { count: 0, .. }))
            ));
            object.ready.store(true, Ordering::Release);
            assert!(matches!(
                tx.poll(Some(first)),
                Poll::Ready(Ok(Reply::Io { count: 4, .. }))
            ));
            assert_eq!(tx.buffer(), b"once");
            drop(tx);
            assert_eq!(*object.wait.lock(), None);
            let mut again = write(descriptor.clone(), 4);
            assert!(matches!(
                again.prepare(None, |buffer| {
                    buffer.copy_from_slice(b"next");
                    Ok(())
                }),
                Poll::Ready(Ok(()))
            ));
            assert_eq!(again.buffer().as_ptr(), identity);
            assert!(matches!(again.poll(None), Poll::Ready(Ok(_))));
            drop(again);
            object.ready.store(false, Ordering::Release);
            let mut abandoned = write(descriptor.clone(), 4);
            assert!(matches!(
                abandoned.prepare(Some(second), |buffer| {
                    buffer.copy_from_slice(b"keep");
                    Ok(())
                }),
                Poll::Ready(Ok(()))
            ));
            assert!(abandoned.poll(Some(second)).is_pending());
            // A real restarted syscall prepares a new exact wait sequence.
            let restarted = token(second.thread().index(), 3);
            assert!(matches!(
                abandoned.prepare(Some(restarted), |_| panic!("restart recopied input")),
                Poll::Ready(Ok(()))
            ));
            assert!(abandoned.poll(Some(restarted)).is_pending());
            assert_eq!(*object.wait.lock(), Some(restarted));
            assert_eq!(object.submissions.load(Ordering::Relaxed), 3);
            assert_eq!(abandoned.buffer(), b"keep");
            drop(abandoned);
            assert_eq!(*object.wait.lock(), None);
        });
        assert_eq!(allocations, (0, 0, 0));
        assert_eq!(object.submissions.load(Ordering::Relaxed), 3);
        assert_eq!(object.resets.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn endpoint_rights_errors_copy_failure_and_final_pending_owner_cleanup() {
        let (descriptor, object, drops) = fixture(false, true);
        assert!(matches!(
            IoTransaction::new(
                descriptor.clone(),
                Operation::Read {
                    offset: 0,
                    limit: 0
                },
                0
            ),
            Err(errno::EBADF)
        ));
        assert!(matches!(
            IoTransaction::new(descriptor.clone(), Operation::Sync, 0),
            Err(errno::EINVAL)
        ));
        assert!(matches!(
            IoTransaction::new(descriptor.clone(), Operation::Readdir { offset: 0 }, 1),
            Err(errno::ENOTDIR)
        ));
        let mut failed = write(descriptor.clone(), 4);
        assert!(matches!(
            failed.prepare(None, |_| Err(errno::EFAULT)),
            Poll::Ready(Err(errno::EFAULT))
        ));
        drop(failed);
        assert_eq!(object.submissions.load(Ordering::Relaxed), 0);
        let mut pending = write(descriptor.clone(), 4);
        assert!(matches!(
            pending.prepare(None, |buffer| {
                buffer.fill(0);
                Ok(())
            }),
            Poll::Ready(Ok(()))
        ));
        assert!(pending.poll(Some(token(0, 1))).is_pending());
        drop(descriptor);
        drop(object);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(pending); // Owns the last descriptor and cancels before final release.
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        let (input, _, _) = fixture(true, false);
        assert!(matches!(
            IoTransaction::new(
                input,
                Operation::Write {
                    offset: 0,
                    append: false
                },
                0
            ),
            Err(errno::EBADF)
        ));
    }
}
