//! Staged device bootstrap over one shared runtime firmware view.

#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
mod interrupts;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
mod storage;
#[cfg(feature = "platform-raspberrypi-3b-plus")]
mod uart;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
mod virtio_mmio;

use super::PlatformInfo;

/// Prepare the devices and metadata required before generic kernel boot.
/// Runs before heap/allocator initialization and performs no heap allocation.
/// # Arguments
/// * `platform` - Published early MMIO ranges, already mapped by bootstrap MMU.
/// * `dtb_pa` - Physical address of the resident firmware blob.
/// * `bytes` - Immutable, permanently mapped bytes of the same blob.
/// * `boot_hardware_id` - Normalized executing boot CPU identity.
/// # Returns
/// Permanent generic boot information. Device probing that needs DMA/heap is
/// deferred to init_devices in the ordinary init task.
/// # Panics
/// Invalid/repeated DTB publication, unsupported UART/CPU/payload metadata,
/// inconsistent CPU counts, or interrupt-controller initialization failure.
/// # Safety
/// Call exactly once on CPU0 with IRQs masked, before scheduler/secondary entry.
/// The firmware bytes must remain reserved and mapped across VM transitions.
pub(crate) unsafe fn init_early(
    platform: &PlatformInfo,
    dtb_pa: usize,
    bytes: &'static [u8],
    boot_hardware_id: u64,
) -> &'static bootinfo::BootInfo {
    let tree = kernel::dtb::init(bytes).expect("invalid runtime DTB");
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    super::uart::runtime::configure_from_ranges(
        uart::parse(tree, platform).expect("invalid Pi UART resources"),
    );
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    super::uart::runtime::configure_from_platform(platform);
    super::init_cpu_topology(tree, boot_hardware_id)
        .unwrap_or_else(|err| panic!("arch: invalid AArch64 CPU topology: {err:?}"));
    super::init_boot_payloads(tree);
    // SAFETY: inherited CPU0-only boot-stage contract and resident blob identity.
    let bootinfo = unsafe { kernel::boot::init_bootinfo(dtb_pa, tree) };
    assert_eq!(
        bootinfo.cpu_count as usize,
        super::cpu_count(),
        "CPU topology mismatch"
    );
    crate::interrupts::init_global(platform);
    bootinfo
}

/// Probe and register optional late devices after runtime VM/heap activation.
/// Call once from CPU0's ordinary init task before namespace publication.
/// May allocate/map and perform bounded device work, with IRQs enabled and
/// without holding shared locks. Pi storage remains unsupported.
/// # Returns
/// Number of registered disks (zero for diskless/Pi), or -1
/// for absent bootstrap, invalid resources, repeated initialization or driver
/// failure. No filesystem is initialized here.
pub(crate) fn init_devices() -> i32 {
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    {
        0
    }
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    {
        let Some(tree) = kernel::dtb::runtime() else {
            return -1;
        };
        storage::init(tree).unwrap_or(-1)
    }
}
