use core::arch::asm;

const CNTP_CTL_ENABLE: u32 = 1 << 0;
const CNTP_CTL_IMASK: u32 = 1 << 1;
const CNTP_CTL_ISTATUS: u32 = 1 << 2;
const CNTKCTL_EL0PCTEN: u64 = 1 << 0;

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
    }
    #[cfg(feature = "wake-profile")]
    kernel::profile::programmed(deadline, effective_deadline);
}

#[inline(always)]
pub unsafe fn disable() {
    unsafe {
        write_ctl(0);
    }
}

/// Initialize the executing CPU's architected physical timer.
///
/// The local timer is disabled, its compare value is moved beyond any stale
/// boot deadline before the PPI is enabled. EL0 physical-counter/frequency
/// reads are enabled, while virtual counter access, timer programming, and
/// event streams remain disabled.
/// This function allocates nothing and leaves DAIF unchanged.
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
/// with local timer programming or IRQ dispatch. No logical CPU binding is
/// required during early CPU0 setup.
pub unsafe fn init_current_cpu() -> bool {
    unsafe {
        // Permit only EL0 physical-counter/frequency reads. Timer programming,
        // virtual-counter access and event streams remain privileged/disabled.
        asm!(
            "msr CNTKCTL_EL1, {access}",
            "isb",
            access = in(reg) CNTKCTL_EL0PCTEN,
            options(nomem, nostack, preserves_flags)
        );
        write_ctl(0);
        write_cval(u64::MAX);
    }

    control() & (CNTP_CTL_ENABLE | CNTP_CTL_IMASK | CNTP_CTL_ISTATUS) == 0
}

/// Dispatch the bounded generic timer IRQ path without diagnostic snapshots.
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
    kernel::time::on_timer_interrupt(context);
}
