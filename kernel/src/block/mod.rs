//! Bounded, exclusively owned sector devices and validated MBR partitions.
//!
//! I/O is ordinary-thread work only: never call it from IRQ, scheduler, timer,
//! or IRQ-disabled/locked paths. Drivers preallocate request storage and bound
//! both request size and completion waits. Errors may follow partial transfers;
//! callers must not assume failed writes were rolled back.

use alloc::boxed::Box;

use crate::sync::SpinLock;

mod mbr;
pub use mbr::{Mbr, PartitionDevice, PartitionEntry};

/// Fixed logical sector size of this interface, in bytes.
pub const SECTOR_SIZE: usize = 512;
/// Maximum sectors in one call, bounding work independently of disk capacity.
pub const MAX_TRANSFER_SECTORS: usize = 128;

/// Explicit failures shared by sector drivers and partition translation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockError {
    /// Buffer length is not a multiple of 512 bytes.
    InvalidBuffer,
    /// Sector arithmetic overflowed or the request exceeds device bounds.
    OutOfBounds,
    /// Request exceeds [`MAX_TRANSFER_SECTORS`].
    TransferTooLarge,
    /// The device rejects writes because it is read-only.
    ReadOnly,
    /// The device reported an I/O failure or malformed completion.
    Io,
    /// Required transport, request, or synchronization support is absent.
    Unsupported,
    /// An architected-counter completion or whole-transfer deadline expired.
    Timeout,
    /// An earlier uncertain DMA completion permanently disabled this device.
    Poisoned,
    /// Invalid MBR signature, flags, partition extent, or overlap.
    InvalidMbr,
}

/// Architecture-neutral, exclusive owner of a bounded 512-byte sector device.
///
/// Implementations must allocate nothing for I/O, validate before submission,
/// bound completion waits and entire transfers, and never retain caller buffers
/// after return (including errors). `Send` transfers ownership, not concurrent
/// access. This interface does not promise atomic multi-sector writes.
pub trait BlockDevice: Send {
    /// Query stable capacity without allocation, I/O, blocking, or IRQ changes.
    ///
    /// # Returns
    /// Number of addressable 512-byte sectors, not bytes.
    fn capacity_sectors(&self) -> u64;

    /// Query media write protection without allocation, I/O or IRQ changes.
    ///
    /// # Returns
    /// `true` when writes are known to be rejected as read-only, allowing
    /// filesystems to reject write opens/creation/truncation before mutation.
    /// The default is `false`; errors can still occur on subsequent writes.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Read whole sectors in ordinary thread context without allocation.
    ///
    /// # Arguments
    /// * `sector` - Starting device-relative sector; capacity is valid only for
    ///   an empty buffer.
    /// * `buffer` - Destination of at most [`MAX_TRANSFER_SECTORS`] sectors.
    /// # Returns
    /// All requested bytes on success; an empty in-range request is a no-op.
    /// # Errors
    /// Returns [`BlockError`] for invalid length/range, oversized transfer,
    /// unsupported operations, I/O failure, timeout, or poisoned state. A failed
    /// read may have changed a prefix of `buffer`.
    fn read_sectors(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError>;

    /// Write whole sectors in ordinary thread context without allocation.
    ///
    /// # Arguments
    /// * `sector` - Starting device-relative sector.
    /// * `buffer` - Source of at most [`MAX_TRANSFER_SECTORS`] sectors.
    /// # Returns
    /// All sectors submitted successfully; empty in-range writes are no-ops.
    /// Persistence additionally requires [`Self::sync`].
    /// # Errors
    /// Returns [`BlockError`] for invalid length/range, oversized transfer,
    /// read-only media, I/O failure, timeout, unsupported operations, or poisoned
    /// state. A failed write may have changed a prefix on disk.
    fn write_sectors(&mut self, sector: u64, buffer: &[u8]) -> Result<(), BlockError>;

    /// Persist prior writes in ordinary thread context, without allocation.
    ///
    /// # Returns
    /// Success only after a bounded durability operation completes, or when
    /// the implementation guarantees writes are already durable.
    /// # Errors
    /// Returns [`BlockError::Unsupported`] when durability cannot be promised,
    /// or the driver's I/O, timeout, or poisoned error.
    fn sync(&mut self) -> Result<(), BlockError>;
}

/// Validate a transfer before submitting any I/O. Allocation-free and bounded.
///
/// # Arguments
/// * `capacity` - Device or partition capacity in sectors.
/// * `sector` - Relative starting sector.
/// * `bytes` - Buffer length, including zero for a no-op.
/// # Returns
/// The number of sectors, including zero for an in-range empty transfer.
/// # Errors
/// Returns invalid-buffer, transfer-too-large, or out-of-bounds errors; checked
/// arithmetic rejects wraparound before any driver observes the request.
pub fn validate_transfer(capacity: u64, sector: u64, bytes: usize) -> Result<usize, BlockError> {
    if !bytes.is_multiple_of(SECTOR_SIZE) {
        return Err(BlockError::InvalidBuffer);
    }
    let count = bytes / SECTOR_SIZE;
    if count > MAX_TRANSFER_SECTORS {
        return Err(BlockError::TransferTooLarge);
    }
    if sector
        .checked_add(count as u64)
        .is_none_or(|end| end > capacity)
    {
        return Err(BlockError::OutOfBounds);
    }
    Ok(count)
}

/// Maximum number of boot-discovered disks, in firmware discovery order.
pub const MAX_BOOT_DEVICES: usize = 32;

static BOOT_DEVICES: SpinLock<[Option<Box<dyn BlockDevice>>; MAX_BOOT_DEVICES]> =
    SpinLock::new([const { None }; MAX_BOOT_DEVICES]);

/// Transfer a boot device into the staging registry, with no I/O or allocation.
///
/// Call only from ordinary initialization thread context, before namespace
/// publication. The short lock moves ownership only and preserves IRQ state.
/// # Arguments
/// * `device` - Exclusive driver owner with all request storage preallocated.
/// # Returns
/// Success after ownership transfers to the registry.
/// # Errors
/// Returns the original `device` unchanged when the registry is full; it
/// is neither destroyed nor used under the registry lock.
pub fn install_boot_device(device: Box<dyn BlockDevice>) -> Result<(), Box<dyn BlockDevice>> {
    let mut slots = BOOT_DEVICES.lock();
    let Some(slot) = slots.iter_mut().find(|slot| slot.is_none()) else {
        return Err(device);
    };
    *slot = Some(device);
    Ok(())
}

/// Extract all staged disks for filesystem initialization outside all locks.
///
/// Ordinary-thread only; does not allocate, perform I/O, or change IRQ state.
/// # Returns
/// Firmware-ordered owners, with empty trailing slots (all empty if consumed
/// or diskless). The lock releases before any owner can be used or destroyed.
pub(crate) fn take_boot_devices() -> [Option<Box<dyn BlockDevice>>; MAX_BOOT_DEVICES] {
    core::mem::replace(
        &mut *BOOT_DEVICES.lock(),
        [const { None }; MAX_BOOT_DEVICES],
    )
}

#[cfg(test)]
mod tests;
