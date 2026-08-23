use core::{
    arch::asm,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::platform::CPU_CAPACITY;

const CNTP_CTL_ENABLE: u32 = 1 << 0;
const CNTP_CTL_IMASK: u32 = 1 << 1;
const CNTP_CTL_ISTATUS: u32 = 1 << 2;

pub const TIMER_IRQ_ID_PHYS: u32 = 30;

#[unsafe(no_mangle)]
pub static TIMER_FREQ_HZ_BY_CPU: [AtomicU64; CPU_CAPACITY] =
    [const { AtomicU64::new(0) }; CPU_CAPACITY];
#[unsafe(no_mangle)]
pub static TIMER_CTL_BY_CPU: [AtomicU64; CPU_CAPACITY] =
    [const { AtomicU64::new(0) }; CPU_CAPACITY];
#[unsafe(no_mangle)]
pub static TIMER_COUNTER_BY_CPU: [AtomicU64; CPU_CAPACITY] =
    [const { AtomicU64::new(0) }; CPU_CAPACITY];
#[unsafe(no_mangle)]
pub static TIMER_NEXT_DEADLINE_BY_CPU: [AtomicU64; CPU_CAPACITY] =
    [const { AtomicU64::new(0) }; CPU_CAPACITY];

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
        record_state(now, effective_deadline);
    }
}

#[inline(always)]
pub unsafe fn disable() {
    unsafe {
        write_ctl(0);
        record_state(counter(), 0);
    }
}

/// Initialize the executing CPU's architected physical timer.
///
/// The local timer is disabled, its compare value is moved beyond any stale
/// boot deadline, and the per-CPU diagnostics are reset before the PPI is
/// enabled. This function allocates nothing and leaves DAIF unchanged.
///
/// # Returns
///
/// Returns `true` when the local control register reads back in the expected
/// disabled, unmasked, and inactive state.
///
/// # Safety
///
/// The caller must execute at EL1 while local IRQ delivery is masked. It must
/// own that CPU's physical timer registers and must not call this concurrently
/// with local timer programming or IRQ dispatch. A logical CPU binding is
/// optional during early CPU0 setup; when absent, only test diagnostics are
/// skipped.
pub unsafe fn init_current_cpu() -> bool {
    unsafe {
        let freq = frequency_hz();
        write_ctl(0);
        write_cval(u64::MAX);
        if let Some(index) = current_logical_index() {
            TIMER_FREQ_HZ_BY_CPU[index].store(freq, Ordering::Relaxed);
        }
        record_state(counter(), 0);
    }

    control() & (CNTP_CTL_ENABLE | CNTP_CTL_IMASK | CNTP_CTL_ISTATUS) == 0
}

/// Record timer diagnostics and dispatch the bounded generic timer IRQ path.
///
/// # Arguments
///
/// * `context` - Exclusive live IRQ return context for scheduler handoff.
///
/// # Returns
///
/// Returns after generic timed-event dispatch and any scheduler frame
/// replacement. The path does not allocate or block.
pub(crate) fn on_timer_irq(context: &mut kernel::arch::ActiveContext<'_>) {
    record_state(counter(), current_deadline());

    kernel::time::on_timer_interrupt(context);
}

#[inline(always)]
fn current_deadline() -> u64 {
    let value: u64;
    // SAFETY: CNTP_CVAL_EL0 is the executing PE's architected compare value.
    unsafe {
        asm!(
            "mrs {value}, CNTP_CVAL_EL0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
fn current_logical_index() -> Option<usize> {
    let encoded: usize;
    // SAFETY: TPIDR_EL1 is the architecture-owned logical binding. Zero is
    // explicitly unbound; nonzero values encode index + 1.
    unsafe {
        asm!(
            "mrs {encoded}, TPIDR_EL1",
            encoded = out(reg) encoded,
            options(nomem, nostack, preserves_flags)
        );
    }
    encoded.checked_sub(1).filter(|index| *index < CPU_CAPACITY)
}

#[inline(always)]
fn record_state(counter: u64, deadline: u64) {
    let Some(index) = current_logical_index() else {
        return;
    };
    TIMER_COUNTER_BY_CPU[index].store(counter, Ordering::Relaxed);
    TIMER_NEXT_DEADLINE_BY_CPU[index].store(deadline, Ordering::Relaxed);
    TIMER_CTL_BY_CPU[index].store(control() as u64, Ordering::Relaxed);
}
