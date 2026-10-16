use crate::{arch::ActiveContext, time::TimedEvent};

use super::{CommitResult, call::SchedCallWaitOutput, commit_wait, prepare_wait};

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

    crate::sched::call::sleep_until_counter(deadline);
}

/// Block the current userspace syscall until an absolute counter deadline.
///
/// Unlike the private kernel sched-call wrapper, an EL0 syscall resumes
/// directly after `svc` and has no suspended kernel-stack continuation to call
/// `finish_wait`. This path therefore uses automatic wait consumption.
///
/// # Arguments
///
/// * `context` - Exclusive live syscall context saved and replaced if the
///   deadline is still in the future.
/// * `deadline` - Absolute architecture counter deadline owned by the current
///   thread's immutable home CPU.
///
/// # Returns
///
/// Returns `true` after applying a blocking handoff and `false` if completion
/// won before commit. The operation uses preallocated deadline and scheduler
/// storage, allocates nothing, and may block the current thread.
/// One scheduler guard covers prepare, timed-event publication, and commit;
/// the time-owner lock is released before the context handoff.
///
/// # Panics
///
/// Panics if scheduler wait invariants fail or the current CPU's bounded time
/// queue is unavailable or exhausted.
pub(crate) fn sleep_user_until_counter(context: &mut ActiveContext<'_>, deadline: u64) -> bool {
    let cpu = super::current_cpu();
    let mut scheduler = super::scheduler_mut();
    let mut local = scheduler.on_cpu(cpu);
    let prepared = local.transition_prepare_automatic_wait();
    let token = prepared.token();
    #[cfg(feature = "wake-profile")]
    crate::profile::bind(token, deadline);
    crate::time::schedule_event(deadline, TimedEvent::wait_deadline(token));
    #[cfg(feature = "wake-profile")]
    crate::profile::wait_published(token);
    match local.commit_prepared_wait(context, prepared) {
        CommitResult::Blocked(_) => true,
        CommitResult::Early(_) => false,
        CommitResult::Stale => {
            crate::time::cancel_event(TimedEvent::wait_deadline(token));
            panic!("sched: userspace sleep wait became stale before commit")
        }
    }
}

#[inline(always)]
pub fn sleep_until(deadline: u64) {
    sleep_until_counter(deadline);
}

/// Block the current thread until an absolute counter deadline.
///
/// # Arguments
///
/// * `context` - Exclusive live sched-call context saved and replaced by the
///   scheduler.
/// * `deadline` - Absolute architecture counter value for the wake event.
/// * `output` - Stack-owned sched-call output that retains the exact token and
///   an optional completion observed before commit.
///
/// # Returns
///
/// Returns after the thread is later resumed. Event insertion uses preallocated
/// storage and this scheduler handoff does not allocate.
pub(crate) fn on_sleep_sync(
    context: &mut ActiveContext<'_>,
    deadline: u64,
    output: &mut SchedCallWaitOutput,
) {
    let prepared = prepare_wait();
    let token = prepared.token();
    output.record_token(token);
    crate::time::schedule_event(deadline, TimedEvent::wait_deadline(token));
    match commit_wait(context, prepared) {
        CommitResult::Blocked(_) => {}
        CommitResult::Early(cause) => output.record_early(cause),
        CommitResult::Stale => panic!("sched: sleep wait became stale before commit"),
    }
    crate::trace!("sched: thread wait {token:?} sleeping until counter {deadline}");
}
