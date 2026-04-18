use core::arch::asm;

const CNTP_CTL_ENABLE: u32 = 1 << 0;
const CNTP_CTL_IMASK: u32 = 1 << 1;
const CNTP_CTL_ISTATUS: u32 = 1 << 2;

pub const TIMER_IRQ_ID_PHYS: u32 = 30;

#[unsafe(no_mangle)]
pub static mut BOOT_TIMER_FREQ_HZ: u64 = 0;
#[unsafe(no_mangle)]
pub static mut BOOT_TIMER_CTL: u64 = 0;
#[unsafe(no_mangle)]
pub static mut BOOT_TIMER_COUNTER: u64 = 0;
#[unsafe(no_mangle)]
pub static mut BOOT_TIMER_NEXT_DEADLINE: u64 = 0;

#[inline(always)]
pub fn frequency_hz() -> u64 {
    let value: u64;
    unsafe {
        asm!(
            "mrs {value}, CNTFRQ_EL0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
pub fn counter() -> u64 {
    let value: u64;
    unsafe {
        asm!(
            "mrs {value}, CNTPCT_EL0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
pub fn control() -> u32 {
    let value: u64;
    unsafe {
        asm!(
            "mrs {value}, CNTP_CTL_EL0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value as u32
}

#[inline(always)]
pub unsafe fn write_cval(deadline: u64) {
    unsafe {
        asm!(
            "msr CNTP_CVAL_EL0, {value}",
            value = in(reg) deadline,
            options(nomem, nostack, preserves_flags)
        );
        asm!("isb", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub unsafe fn write_ctl(value: u32) {
    unsafe {
        asm!(
            "msr CNTP_CTL_EL0, {value}",
            value = in(reg) value as u64,
            options(nomem, nostack, preserves_flags)
        );
        asm!("isb", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub unsafe fn arm_deadline(deadline: u64) {
    let now = counter();
    let effective_deadline = if deadline <= now {
        now.saturating_add(1)
    } else {
        deadline
    };

    unsafe {
        write_cval(effective_deadline);
        write_ctl(CNTP_CTL_ENABLE);
        BOOT_TIMER_COUNTER = now;
        BOOT_TIMER_NEXT_DEADLINE = effective_deadline;
        BOOT_TIMER_CTL = control() as u64;
    }
}

#[inline(always)]
pub unsafe fn disable() {
    unsafe {
        write_ctl(0);
        BOOT_TIMER_COUNTER = counter();
        BOOT_TIMER_NEXT_DEADLINE = 0;
        BOOT_TIMER_CTL = control() as u64;
    }
}

/// Early timer setup for the one-shot deadline engine.
///
/// This is intentionally observable from GDB:
/// - BOOT_TIMER_FREQ_HZ
/// - BOOT_TIMER_CTL
/// - BOOT_TIMER_COUNTER
/// - BOOT_TIMER_NEXT_DEADLINE
pub unsafe fn early_init() {
    unsafe {
        let freq = frequency_hz();
        BOOT_TIMER_FREQ_HZ = freq;
        BOOT_TIMER_COUNTER = counter();
        BOOT_TIMER_NEXT_DEADLINE = 0;
        disable();
    }

    let _ = CNTP_CTL_IMASK;
    let _ = CNTP_CTL_ISTATUS;
}

#[inline(always)]
pub unsafe fn enable_cpu_irq() {
    unsafe {
        // Clear IRQ mask bit in DAIF.
        asm!("msr daifclr, #2", options(nomem, nostack, preserves_flags));
        asm!("isb", options(nomem, nostack, preserves_flags));
    }
}

pub fn on_timer_irq(frame_words: *mut u64) {
    unsafe {
        BOOT_TIMER_COUNTER = counter();
        BOOT_TIMER_CTL = control() as u64;
    }

    kernel::time::on_timer_interrupt(frame_words);
}
