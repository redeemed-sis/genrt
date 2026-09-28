//! BCM2837 password-protected PM watchdog and Raspberry Pi firmware halt flag.
//! Halt requests partition code 63 in PM_RSTS, then a full watchdog reset;
//! VideoCore firmware stops booting. This does not disconnect the 5 V supply.

use crate::{mmio, mmu};

const PM: usize = 0x3f10_0000;
const RSTC: usize = PM + 0x1c;
const RSTS: usize = PM + 0x20;
const WDOG: usize = PM + 0x24;
const PASSWORD: u32 = 0x5a00_0000;
const RESET_CONFIG_MASK: u32 = 0x30;
const FULL_RESET: u32 = 0x20;
const FIRMWARE_HALT: u32 = 0x555;
const RESET_TICKS: u32 = 10; // Watchdog ticks at 65536 Hz: about 153 us.

fn read(pa: usize) -> u32 {
    // SAFETY: the PM window has an architecture-owned Device mapping.
    unsafe { mmio::mmio_read32(mmu::phys_to_hva(pa)) }
}
fn write(pa: usize, value: u32) {
    // SAFETY: the terminal request claimant owns watchdog configuration.
    unsafe { mmio::mmio_write32(mmu::phys_to_hva(pa), PASSWORD | (value & 0x00ff_ffff)) }
}

fn arm(halt: bool) -> u32 {
    if halt {
        write(RSTS, read(RSTS) | FIRMWARE_HALT);
    }
    let previous = read(RSTC);
    write(WDOG, RESET_TICKS);
    write(RSTC, (previous & !RESET_CONFIG_MASK) | FULL_RESET);
    previous
}

/// Request board reset (`halt == false`) or firmware halt (`halt == true`).
///
/// # Returns
/// Success never returns. Returns architecture failure code 1 if no reset was
/// observed within 10 ms, or denial code 3 for a concurrent request. Allocates
/// nothing, masks local IRQ during the bounded attempt, and restores it on
/// failure. The watchdog resets all CPUs; no separate PSCI CPU_OFF is needed.
#[cfg(not(test))]
pub(crate) fn request(halt: bool) -> i64 {
    use core::{
        arch::asm,
        sync::atomic::{AtomicBool, Ordering},
    };
    static CLAIMED: AtomicBool = AtomicBool::new(false);
    if CLAIMED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return 3;
    }
    let irq = crate::arch_local_irq_save_and_disable();
    let previous_status = read(RSTS);
    let previous_control = arm(halt);
    let start: u64;
    let frequency: u64;
    // SAFETY: physical counter access was enabled by the architecture entry.
    unsafe {
        asm!("dsb sy", "mrs {start}, CNTPCT_EL0", "mrs {frequency}, CNTFRQ_EL0",
            start = out(reg) start, frequency = out(reg) frequency, options(nostack));
    }
    loop {
        let now: u64;
        // SAFETY: read-only monotonic physical counter, no memory effects.
        unsafe { asm!("mrs {now}, CNTPCT_EL0", now = out(reg) now, options(nomem, nostack)) };
        if now.wrapping_sub(start) >= frequency / 100 {
            break;
        }
        core::hint::spin_loop();
    }
    // An unimplemented watchdog must not leave a latent reset or halt flag.
    write(RSTC, previous_control & !RESET_CONFIG_MASK);
    write(RSTS, previous_status);
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
    CLAIMED.store(false, Ordering::Release);
    crate::arch_local_irq_restore(irq);
    1
}

#[cfg(test)]
mod tests {
    #[test]
    fn reset_and_halt_write_password_and_preserve_unrelated_bits() {
        for halt in [false, true] {
            crate::reset_power_registers();
            super::arm(halt);
            let mut expected = Vec::new();
            if halt {
                expected.push((0x3f100020, 0x5a000d55));
            }
            expected.extend([(0x3f100024, 0x5a00000a), (0x3f10001c, 0x5a000120)]);
            crate::assert_power_writes(&expected);
        }
    }
}
