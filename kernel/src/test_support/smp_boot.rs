//! Test-only coordinator for parked secondary CPU bring-up.

use crate::{cpu, sched};

use super::protocol;

const SUITE: &str = "smp-boot";

/// Single CPU0 coordinator selected by the SMP boot QEMU feature.
pub(crate) const THREADS: [sched::StaticThread; 1] = [sched::StaticThread::new(
    coordinator,
    sched::ThreadArg::empty(),
)];

fn coordinator(_arg: sched::ThreadArg) -> usize {
    if !cpu::secondary_boot_complete() {
        protocol::fail("secondary-cpus-parked", "INCOMPLETE_STARTUP");
    }
    protocol::ready(SUITE);
    protocol::case_start("secondary-cpus-parked");
    cpu::validate_smp_boot_for_test();
    sched::validate_secondary_contexts_offline_for_test(cpu::registered_count());
    protocol::pass("secondary-cpus-parked");
    protocol::done(SUITE)
}
