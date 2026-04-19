use bootinfo::{BootInfo, MemoryRegion, MemoryRegionKind};

use crate::dtb::{MAX_BOOT_MEMORY_REGIONS, parse_memory_regions};

static mut BOOT_INFO: BootInfo = BootInfo::new();
static mut BOOT_MEMORY_MAP: [MemoryRegion; MAX_BOOT_MEMORY_REGIONS] = [MemoryRegion {
    start: 0,
    size: 0,
    kind: MemoryRegionKind::Reserved,
}; MAX_BOOT_MEMORY_REGIONS];

/// # Safety
/// Must be called exactly once during early boot, before interrupts and SMP are enabled.
pub unsafe fn init_bootinfo(dtb_pa: usize) -> &'static BootInfo {
    unsafe {
        crate::debug!("bootinfo: parsing dtb");
        let parsed = parse_memory_regions(dtb_pa, &mut *core::ptr::addr_of_mut!(BOOT_MEMORY_MAP))
            .unwrap_or_else(|err| panic!("bootinfo: failed to parse DTB memory map: {err:?}"));
        BOOT_INFO.dtb_pa = dtb_pa as u64;
        BOOT_INFO.dtb_size = parsed.dtb_size;
        BOOT_INFO.memory_map = if parsed.region_count == 0 {
            &[]
        } else {
            &(&*core::ptr::addr_of!(BOOT_MEMORY_MAP))[..parsed.region_count]
        };
        crate::debug!(
            "bootinfo: dtb regions={} size={} bytes",
            parsed.region_count,
            parsed.dtb_size
        );
        &*core::ptr::addr_of!(BOOT_INFO)
    }
}
