//! PSCI calls through the QEMU DTB-selected HVC conduit.

use core::arch::asm;

const PSCI_0_2_FN_BASE: u64 = 0x8400_0000;
const PSCI_SYSTEM_OFF: u64 = PSCI_0_2_FN_BASE + 8;
const PSCI_SYSTEM_RESET: u64 = PSCI_0_2_FN_BASE + 9;
const PSCI_NOT_SUPPORTED: i64 = -1;
const PSCI_DENIED: i64 = -3;
const ARCH_POWER_FAILED: i64 = 1;
const ARCH_POWER_NOT_SUPPORTED: i64 = 2;
const ARCH_POWER_DENIED: i64 = 3;

/// Invoke PSCI `CPU_ON` through the HVC conduit.
///
/// # Arguments
///
/// * `function_id` - DTB-provided PSCI `CPU_ON` function ID.
/// * `target` - Raw target affinity value from the CPU DTB node.
/// * `entry` - Physical secondary entry address.
/// * `context` - Bootstrap slot returned to the secondary entry in `x0`.
///
/// # Returns
///
/// Returns the signed PSCI status from `x0`.
///
/// This architecture-local call is allocation-free, does not block in kernel
/// code, and preserves the caller's IRQ mask.
pub(crate) fn cpu_on(function_id: u64, target: u64, entry: u64, context: u64) -> i64 {
    call(function_id, target, entry, context)
}

/// Request a system-wide PSCI reset through the HVC conduit.
///
/// # Returns
///
/// Success is terminal. A returned PSCI status is translated to the narrow
/// generic architecture-hook error ABI.
///
/// The call is allocation-free, acquires no locks, and preserves IRQ mask state.
pub(crate) fn system_reset() -> i64 {
    power_result(call(PSCI_SYSTEM_RESET, 0, 0, 0))
}

/// Request a system-wide PSCI power off through the HVC conduit.
///
/// # Returns
///
/// Success is terminal. A returned PSCI status is translated to the narrow
/// generic architecture-hook error ABI.
///
/// The call is allocation-free, acquires no locks, and preserves IRQ mask state.
pub(crate) fn system_off() -> i64 {
    power_result(call(PSCI_SYSTEM_OFF, 0, 0, 0))
}

fn power_result(status: i64) -> i64 {
    match status {
        PSCI_NOT_SUPPORTED => ARCH_POWER_NOT_SUPPORTED,
        PSCI_DENIED => ARCH_POWER_DENIED,
        _ => ARCH_POWER_FAILED,
    }
}

fn call(function_id: u64, arg0: u64, arg1: u64, arg2: u64) -> i64 {
    let mut status = function_id;
    // SAFETY: the platform topology parser accepted the HVC conduit. Register
    // assignment follows the PSCI SMCCC convention, and all clobbered argument
    // registers are explicit outputs.
    unsafe {
        asm!(
            "hvc #0",
            inout("x0") status,
            inout("x1") arg0 => _,
            inout("x2") arg1 => _,
            inout("x3") arg2 => _,
            options(nostack)
        );
    }
    status as i64
}
