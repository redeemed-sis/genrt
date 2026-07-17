use crate::{arch::ActiveContext, task_call::TaskCallWaitOutput, time::TimedEvent};

use super::{CommitResult, WaitKind, commit_wait, prepare_wait};

pub fn usleep(us: u64) {
    if us == 0 {
        return;
    }

    let deadline = crate::time::now_counter().wrapping_add(crate::time::us_to_counts(us));
    sleep_until_counter(deadline);
}

pub fn msleep(ms: u64) {
    if ms == 0 {
        return;
    }

    let deadline = crate::time::now_counter().wrapping_add(crate::time::ms_to_counts(ms));
    sleep_until_counter(deadline);
}

pub fn sleep_until_counter(deadline: u64) {
    if deadline <= crate::time::now_counter() {
        return;
    }

    crate::task_call::sleep_until_counter(deadline);
}

#[inline(always)]
pub fn sleep_until(deadline: u64) {
    sleep_until_counter(deadline);
}

/// Block the current task until an absolute counter deadline.
///
/// # Arguments
///
/// * `context` - Exclusive live task-call context saved and replaced by the
///   scheduler.
/// * `deadline` - Absolute architecture counter value for the wake event.
/// * `output` - Stack-owned task-call output that retains the exact token and
///   an optional completion observed before commit.
///
/// # Returns
///
/// Returns after the task is later resumed. Event insertion uses preallocated
/// storage and this scheduler handoff does not allocate.
pub(crate) fn on_sleep_sync(
    context: &mut ActiveContext<'_>,
    deadline: u64,
    output: &mut TaskCallWaitOutput,
) {
    let prepared = prepare_wait(WaitKind::Deadline);
    let token = prepared.token();
    output.record_token(token);
    crate::time::schedule_event(deadline, TimedEvent::WaitDeadline(token));
    match commit_wait(context, prepared) {
        CommitResult::Blocked(_) => {}
        CommitResult::Early(cause) => output.record_early(cause),
        CommitResult::Stale => panic!("sched: sleep wait became stale before commit"),
    }
    crate::trace!("sched: task wait {token:?} sleeping until counter {deadline}");
}
