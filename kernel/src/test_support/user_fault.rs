//! Test-only coordinator for lower-EL fault classification and process cleanup.

use crate::{
    process::{self, ProcessExitStatus, UserFaultKind},
    sched::{self, ThreadArg},
};

use super::protocol;

const SUITE: &str = "user-fault";

/// Single coordinator selected by the user-fault QEMU feature.
pub(crate) const THREADS: [sched::StaticThread; 1] =
    [sched::StaticThread::new(coordinator, ThreadArg::empty())];

fn coordinator(_arg: ThreadArg) -> usize {
    protocol::ready(SUITE);
    protocol::case_start("null-data-abort");
    let pid = process::spawn_first_user_process()
        .unwrap_or_else(|_| protocol::fail("null-data-abort", "SPAWN"));
    match process::process_join(pid) {
        Ok(ProcessExitStatus::Faulted(fault))
            if fault.kind == UserFaultKind::DataTranslationFault =>
        {
            protocol::pass("null-data-abort");
        }
        _ => protocol::fail("null-data-abort", "WRONG_FAULT"),
    }
    protocol::done(SUITE)
}
