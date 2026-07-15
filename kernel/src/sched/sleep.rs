use crate::{arch::ActiveContext, time::TimedEvent};

use super::{Scheduler, preempt::BlockReason, scheduler_mut};

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
///
/// # Returns
///
/// Returns after the task is later resumed. Event insertion uses preallocated
/// storage and this scheduler handoff does not allocate.
pub(crate) fn on_sleep_sync(context: &mut ActiveContext<'_>, deadline: u64) {
    scheduler_mut().block_current_until(context, deadline);
}

impl Scheduler {
    fn block_current_until(&mut self, context: &mut ActiveContext<'_>, deadline: u64) {
        let (current, next) = self.begin_block_current(context, BlockReason::Sleep);
        crate::time::schedule_event(deadline, TimedEvent::WakeTask(current));
        self.finish_block_current(current, next);
        crate::trace!("sched: task {current} sleeping until counter {deadline}");
    }
}
