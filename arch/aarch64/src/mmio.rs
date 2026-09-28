#[inline(always)]
pub(crate) unsafe fn mmio_write32(addr: usize, value: u32) {
    // SAFETY: Caller guarantees `addr` is a valid MMIO register address.
    unsafe {
        core::arch::asm!(
            "str {value:w}, [{addr}]",
            addr = in(reg) addr,
            value = in(reg) value,
            options(nostack, preserves_flags)
        );
    }
}

#[inline(always)]
pub(crate) unsafe fn mmio_read32(addr: usize) -> u32 {
    let value: u32;
    // SAFETY: Caller guarantees `addr` is a valid MMIO register address.
    unsafe {
        core::arch::asm!(
            "ldr {value:w}, [{addr}]",
            addr = in(reg) addr,
            value = lateout(reg) value,
            options(nostack, preserves_flags)
        );
    }
    value
}
