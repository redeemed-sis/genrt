//! Test-only coordinator for interrupt-ready secondary CPU bring-up.

use crate::{cpu, sched, time};

use super::protocol;

const SUITE: &str = "smp-boot";
const TIMER_PROBE_TIMEOUT_MS: u64 = 2_000;

/// Single CPU0 coordinator selected by the SMP boot QEMU feature.
pub(crate) const THREADS: [sched::StaticThread; 1] = [sched::StaticThread::new(
    coordinator,
    sched::ThreadArg::empty(),
)];

fn coordinator(_arg: sched::ThreadArg) -> usize {
    if !cpu::secondary_boot_complete() {
        protocol::fail("per-cpu-interrupt-state", "INCOMPLETE_STARTUP");
    }
    protocol::ready(SUITE);
    protocol::case_start("per-cpu-interrupt-state");
    cpu::validate_smp_boot_for_test();
    cpu::validate_smp_device_routing_for_test();
    sched::validate_secondary_contexts_offline_for_test(cpu::registered_count());
    protocol::pass("per-cpu-interrupt-state");

    protocol::case_start("per-cpu-timer-irqs");
    time::arm_local_timer_probe_for_test();
    let cpu_count = cpu::registered_count();
    let deadline = time::uptime_ms().saturating_add(TIMER_PROBE_TIMEOUT_MS);
    while !time::local_timer_probes_complete_for_test(cpu_count) {
        if time::uptime_ms() >= deadline {
            protocol::fail("per-cpu-timer-irqs", "TIMEOUT");
        }
        core::hint::spin_loop();
    }
    sched::validate_secondary_contexts_offline_for_test(cpu_count);
    protocol::pass("per-cpu-timer-irqs");
    protocol::done(SUITE)
}
