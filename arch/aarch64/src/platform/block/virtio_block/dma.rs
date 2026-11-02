//! QEMU virt's coherent DMA and direct physical-address HAL.

use core::ptr::NonNull;
use kernel::memory::{self, FrameRange, PAGE_SIZE, vm};
use virtio_drivers::{BufferDirection, Hal};

/// Private HAL used only by this driver's persistent queue/request allocations.
pub(super) struct Dma;

// SAFETY: QEMU virt provides coherent DMA in directly mapped normal RAM and
// uses guest physical addresses without an IOMMU. Queue and request allocations
// are exclusive contiguous physical-frame owners. Only these persistent pages
// reach share/unshare; no stack/heap caller buffer is ever shared with DMA.
unsafe impl Hal for Dma {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (u64, NonNull<u8>) {
        let Some(range) = memory::alloc_contiguous_frames(pages) else {
            // The queue crate recognizes PA zero as allocation failure and
            // never dereferences the accompanying sentinel pointer.
            return (0, NonNull::dangling());
        };
        memory::zero_phys_range(range);
        (
            range.start as u64,
            NonNull::new(vm::phys_to_virt(range.start) as *mut u8).unwrap(),
        )
    }

    unsafe fn dma_dealloc(pa: u64, _va: NonNull<u8>, pages: usize) -> i32 {
        memory::free_contiguous_frames(FrameRange {
            start: pa as usize,
            end: pa as usize + pages * PAGE_SIZE,
        });
        0
    }

    unsafe fn mmio_phys_to_virt(pa: u64, _size: usize) -> NonNull<u8> {
        // This backend never uses PCI BAR mapping. Bootstrap validates and
        // permanently maps MMIO before the driver creates its transport.
        NonNull::new(vm::phys_to_virt(pa as usize) as *mut u8).unwrap()
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> u64 {
        // SAFETY: only our contiguous DMA pages reach this private HAL. HVA is
        // an explicit direct alias, so subtraction returns the real frame PA,
        // not an HVA masquerading as a physical device address.
        let va = buffer.as_ptr() as *mut u8 as usize;
        vm::virt_to_phys_direct(va).unwrap_or_else(|| {
            panic!(
                "virtio-block: DMA direct alias va={va:#x} len={}",
                buffer.len()
            )
        }) as u64
    }

    unsafe fn unshare(_pa: u64, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
        // Queue acquire/release fences suffice for QEMU's coherent DMA RAM.
    }
}
