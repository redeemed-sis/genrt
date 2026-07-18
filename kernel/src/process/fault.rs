use crate::arch::ActiveContext;

use super::{
    error::ProcessFaultError,
    lifecycle::finish_current_process,
    state::{ProcessExitStatus, UserFault},
};

/// Attribute a lower-EL fault to the current process and switch it out.
///
/// # Arguments
///
/// * `context` - Exclusive live lower-EL fault context replaced by scheduler exit.
/// * `fault` - Architecture-classified userspace fault record.
///
/// # Returns
///
/// Returns `Ok(())` after storing fault status, waking registered waiters, and replacing the live context. The path is bounded and does not allocate.
///
/// # Errors
///
/// Returns [`ProcessFaultError::NoCurrentProcess`] when no user process is running or [`ProcessFaultError::InvalidProcess`] for stale process state.
///
/// # Panics
///
/// Panics when a `crate::sync::preempt::PreemptGuard` is active before terminal state changes.
pub fn kill_current_process_on_user_fault(
    context: &mut ActiveContext<'_>,
    fault: UserFault,
) -> Result<(), ProcessFaultError> {
    crate::sync::preempt::assert_preemption_enabled("process fault exit state change");
    finish_current_process(context, ProcessExitStatus::Faulted(fault), usize::MAX)
}
