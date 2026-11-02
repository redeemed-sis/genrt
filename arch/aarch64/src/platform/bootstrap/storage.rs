//! Ordinary-init-thread resource validation, mapping and block-driver binding.

use super::{interrupts::GicV2Domain, virtio_mmio};
use crate::platform::{self, block::virtio_block};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};
use kernel::dtb::{MmioRange, RuntimeDtb};
use kernel::memory::vm;

static INITIALIZED: AtomicBool = AtomicBool::new(false);

// The runtime kernel VM maps L2 blocks, not L3 pages; see mmu/hva.rs.
// This is mapping granularity, never a device address or transport stride.
const MMIO_MAPPING_SIZE: usize = 2 * 1024 * 1024;

/// Bind all supported block controllers in validated firmware traversal order.
/// CPU0's ordinary init thread is the sole caller; may allocate/map with IRQs
/// enabled, without holding shared locks across device initialization.
/// # Arguments
/// * `tree` - Shared resident firmware reader retained by early high-side boot.
/// # Returns
/// Number of block owners installed in the bounded boot registry, or zero.
/// # Errors
/// Returns `()` for repeated initialization, invalid/conflicting resources,
/// enabled device interrupts, mapping/driver failure or a full registry.
pub(super) fn init(tree: &RuntimeDtb<'_>) -> Result<i32, ()> {
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return Err(());
    }
    let platform = platform::info().ok_or(())?;
    let (gicd, gicc) = platform.interrupts.ranges();
    let gicc = gicc.ok_or(())?;
    let interrupts = GicV2Domain::discover(
        tree,
        MmioRange {
            start: gicd.start,
            size: gicd.size,
        },
        MmioRange {
            start: gicc.start,
            size: gicc.size,
        },
    )
    .map_err(|_| ())?;
    let candidates = virtio_mmio::discover(tree, &interrupts).map_err(|_| ())?;
    prepare_mmio(&candidates, platform)?;
    let mut count = 0;
    for candidate in candidates.as_slice() {
        let base = NonNull::new(vm::phys_to_virt(candidate.base) as *mut u8).ok_or(())?;
        // SAFETY: bootstrap validated unique, nonoverlapping firmware windows,
        // permanently mapped them as Device memory, and checked masked SPIs.
        // QEMU virt provides coherent direct-PA DMA. No other owner uses them.
        if let Some(device) =
            unsafe { virtio_block::probe(base, candidate.size) }.map_err(|_| ())?
        {
            kernel::block::install_boot_device(device).map_err(|_| ())?;
            count += 1;
        }
    }
    Ok(count)
}

fn prepare_mmio(
    candidates: &virtio_mmio::Candidates,
    platform: &platform::PlatformInfo,
) -> Result<(), ()> {
    let mut mapped = [0usize; 64];
    let mut mapped_count = 0;
    for candidate in candidates.as_slice() {
        let end = candidate.base.checked_add(candidate.size).ok_or(())?;
        let overlaps = |range: platform::DeviceRange| {
            (candidate.base as u64) < range.start + range.size && range.start < end as u64
        };
        let (gicd, gicc) = platform.interrupts.ranges();
        if end > (1usize << 48)
            || overlaps(platform.ram)
            || overlaps(platform.uart)
            || overlaps(gicd)
            || gicc.is_some_and(overlaps)
        {
            return Err(());
        }
        if !platform::gic::spi_is_masked(candidate.irq) {
            return Err(());
        }
        let first = candidate.base & !(MMIO_MAPPING_SIZE - 1);
        let last = (end - 1) & !(MMIO_MAPPING_SIZE - 1);
        for page in (first..=last).step_by(MMIO_MAPPING_SIZE) {
            if mapped[..mapped_count].contains(&page) {
                continue;
            }
            let va = vm::phys_to_virt(page);
            // SAFETY: validated firmware MMIO outside RAM and existing platform
            // windows. CPU0 init owns these permanent Device mappings. Distinct
            // transports may share a runtime L2 block: map it only once and
            // never remap/unmap it. Existing mappings are rejected by VM.
            unsafe {
                vm::map_kernel_region(
                    va,
                    page,
                    MMIO_MAPPING_SIZE,
                    vm::VmMemoryAttr::DeviceNgRnE,
                    vm::VmFlags::READ
                        .union(vm::VmFlags::WRITE)
                        .union(vm::VmFlags::GLOBAL),
                )
            }
            .map_err(|_| ())?;
            *mapped.get_mut(mapped_count).ok_or(())? = page;
            mapped_count += 1;
        }
    }
    Ok(())
}
