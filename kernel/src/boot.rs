use bootinfo::BootInfo;

static mut BOOT_INFO: BootInfo = BootInfo::new();

/// # Safety
/// Must be called exactly once during early boot, before interrupts and SMP are enabled.
pub unsafe fn init_bootinfo(dtb_pa: usize) -> &'static BootInfo {
    unsafe {
        BOOT_INFO.dtb_pa = dtb_pa as u64;
        &*core::ptr::addr_of!(BOOT_INFO)
    }
}
