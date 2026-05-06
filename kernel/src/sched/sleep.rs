use crate::time::TimedEvent;

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

pub(crate) fn on_sleep_sync(active_frame_words: *mut u64, deadline: u64) {
    scheduler_mut().block_current_until(active_frame_words, deadline);
}

impl Scheduler {
    fn block_current_until(&mut self, active_frame_words: *mut u64, deadline: u64) {
        let (current, next) = self.begin_block_current(active_frame_words, BlockReason::Sleep);
        crate::time::schedule_event(deadline, TimedEvent::WakeTask(current));
        self.finish_block_current(current, next);
        crate::trace!("sched: task {current} sleeping until counter {deadline}");
    }
}
