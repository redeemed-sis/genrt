//! Modern virtio-mmio block controller with QEMU-coherent DMA backing.

mod dma;
mod queue;

use alloc::boxed::Box;
use core::ptr::NonNull;
use kernel::block::{BlockDevice, BlockError};
use virtio_drivers::transport::mmio::{MmioTransport, MmioVersion, VirtIOHeader};

/// Identify a mapped block controller, negotiate it and provision fixed DMA.
/// Ordinary-thread initialization only; may allocate but does not alter IRQs.
/// # Arguments
/// * `base` - Permanently mapped, aligned register-window address.
/// * `size` - Validated byte length of that register window.
/// # Returns
/// An exclusive block owner, or `None` for a non-block virtio device. Unrelated
/// devices are not reset or otherwise claimed.
/// # Errors
/// I/O for an invalid transport or queue/allocation failure, unsupported for
/// legacy transports/features, or timeout during reset/configuration waits.
/// # Safety
/// `base` must expose `size` bytes of valid Device-mapped virtio MMIO registers
/// for the driver's lifetime. The block controller must have no other owner;
/// its platform interrupt must remain masked. DMA must be QEMU-coherent RAM
/// addressed directly by guest physical addresses, without an IOMMU.
pub(crate) unsafe fn probe(
    base: NonNull<u8>,
    size: usize,
) -> Result<Option<Box<dyn BlockDevice>>, BlockError> {
    let header = base.cast::<VirtIOHeader>();
    // SAFETY: the caller supplies a valid mapped register window. Inspect the
    // read-only device ID before constructing a transport: its Drop resets
    // the device, which must not happen for an unrelated controller.
    let id = unsafe { header.as_ptr().cast::<u32>().add(2).read_volatile() };
    if id != 2 {
        return Ok(None);
    }
    // SAFETY: the caller transfers exclusive access to this resident window.
    let transport = unsafe { MmioTransport::new(header, size) }.map_err(|_| BlockError::Io)?;
    if transport.version() != MmioVersion::Modern {
        return Err(BlockError::Unsupported);
    }
    let device = queue::VirtioBlock::<dma::Dma, _>::new(
        transport,
        crate::timer::counter,
        crate::timer::frequency_hz(),
    )?;
    Ok(Some(Box::new(device)))
}
