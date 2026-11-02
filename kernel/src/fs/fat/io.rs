//! Sector adapter and latched device errors for the persistent FAT volume.

use super::shared::{DiskWaiters, SharedDisk};
use crate::{
    block::{BlockDevice, BlockError, MAX_TRANSFER_SECTORS, PartitionEntry, validate_transfer},
    errno::Errno,
    sched::WaitToken,
};
use alloc::{boxed::Box, sync::Arc};
use core::task::Poll;

/// Exclusive partition owner, retained by the filesystem across operations.
pub(super) struct Disk {
    device: Option<Box<dyn BlockDevice>>,
    parent: Option<Arc<SharedDisk>>,
    start: u64,
    capacity: u64,
    readonly: bool,
    pub(super) failure: Option<Errno>,
}

impl Disk {
    /// Wrap a device without I/O, allocation, locks or IRQ-state changes.
    /// # Arguments
    /// * `device` - Exclusive partition-relative sector owner.
    /// # Returns
    /// Ready adapter with no saved device failure.
    #[cfg(test)]
    pub(super) fn new(device: Box<dyn BlockDevice>) -> Self {
        Self {
            capacity: device.capacity_sectors(),
            readonly: device.is_read_only(),
            device: Some(device),
            parent: None,
            start: 0,
            failure: None,
        }
    }

    /// Prepare partition translation without reserving or accessing the parent.
    /// Task-context only; no allocation, I/O or lock acquisition.
    /// # Arguments
    /// * `parent` - Shared physical owner whose stable properties are cached.
    /// * `entry` - Nonempty validated primary partition extent.
    /// * `readonly` - Additional mount-level write prohibition.
    /// # Returns
    /// Detached adapter; reserve before all library calls.
    /// # Errors
    /// `EINVAL` for zero, overflowing or out-of-device extents.
    pub(super) fn partition(
        parent: Arc<SharedDisk>,
        entry: PartitionEntry,
        readonly: bool,
    ) -> Result<Self, Errno> {
        if entry.start_lba == 0
            || entry.sectors == 0
            || entry
                .start_lba
                .checked_add(entry.sectors)
                .is_none_or(|end| end > parent.capacity_sectors())
        {
            return Err(crate::errno::EINVAL);
        }
        Ok(Self {
            readonly: readonly || parent.is_read_only(),
            device: None,
            parent: Some(parent),
            start: entry.start_lba,
            capacity: entry.sectors,
            failure: None,
        })
    }

    /// Reserve the physical owner before entering any resident library code.
    /// # Arguments
    /// * `wake` - Exact waiter for contention, or none during exclusive boot.
    /// # Returns
    /// Ready ownership or registered `Pending`, without I/O or allocation.
    /// # Errors
    /// `EAGAIN` when busy without a token; never latches a media failure.
    pub(super) fn acquire(&mut self, wake: Option<WaitToken>) -> Poll<Result<(), Errno>> {
        if self.device.is_some() {
            return Poll::Ready(Ok(()));
        }
        match self.parent.as_ref().unwrap().acquire(wake) {
            Poll::Ready(Ok(device)) => {
                self.device = Some(device);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    /// Restore a shared physical owner outside the volume lock, without I/O.
    /// # Returns
    /// Claimed physical waiters, or none for an owned/detached adapter. Wake
    /// only after publishing volume availability. No allocation or blocking.
    pub(super) fn release(&mut self) -> Option<DiskWaiters> {
        self.parent
            .as_ref()
            .and_then(|parent| self.device.take().map(|device| parent.restore(device)))
    }

    /// Query cached effective mount/media protection while detached.
    /// # Returns
    /// Readonly status without locks, allocation, I/O, or blocking.
    pub(super) fn is_read_only(&self) -> bool {
        self.readonly
    }

    /// Apply final boot mount policy before namespace publication.
    /// # Arguments
    /// * `readonly` - Requested policy; cannot override physical protection.
    /// # Returns
    /// Nothing; cached policy changes only, without I/O or allocation.
    pub(super) fn set_read_only(&mut self, readonly: bool) {
        self.readonly = readonly || self.parent.as_ref().is_some_and(|p| p.is_read_only());
    }

    fn ensure_usable(&self) -> Result<(), BlockError> {
        if let Some(error) = self.failure {
            return Err(if error == crate::errno::ETIMEDOUT {
                BlockError::Timeout
            } else {
                BlockError::Poisoned
            });
        }
        Ok(())
    }

    fn checked(&self, sector: u64, bytes: usize) -> Result<(), BlockError> {
        self.ensure_usable()?;
        validate_transfer(self.capacity, sector, bytes).map(|_| ())
    }

    fn retain(&mut self, result: Result<(), BlockError>) -> Result<(), BlockError> {
        if let Err(error) = result {
            self.failure = Some(super::block_errno(error));
        }
        result
    }

    /// Complete explicit device durability after filesystem metadata publication.
    /// # Returns
    /// Success after bounded task-context device sync; allocation-free, unlocked.
    /// # Errors
    /// Latched device failure or transport sync error; failures remain latched.
    pub(super) fn sync(&mut self) -> Result<(), BlockError> {
        self.ensure_usable()?;
        let result = self.device.as_mut().unwrap().sync();
        self.retain(result)
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        // Mount consumes Disk and drops it on failure. Return the reservation
        // on that path too; neither successful nor failed drop performs I/O.
        if let Some(waiters) = self.release() {
            waiters.wake();
        }
    }
}

impl resident_fat::BlockDevice for Disk {
    type Error = BlockError;

    fn read(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        self.checked(sector, buffer.len())?;
        let result = self
            .device
            .as_mut()
            .unwrap()
            .read_sectors(self.start + sector, buffer);
        self.retain(result)
    }

    fn write(&mut self, sector: u64, buffer: &[u8]) -> Result<(), BlockError> {
        self.checked(sector, buffer.len())?;
        if self.readonly {
            return Err(BlockError::ReadOnly);
        }
        let result = self
            .device
            .as_mut()
            .unwrap()
            .write_sectors(self.start + sector, buffer);
        self.retain(result)
    }

    fn block_count(&mut self) -> Result<Option<u64>, BlockError> {
        Ok(Some(self.capacity))
    }

    fn max_transfer_blocks(&self) -> u64 {
        MAX_TRANSFER_SECTORS as u64
    }
}
