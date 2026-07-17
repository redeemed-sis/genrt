use core::cell::UnsafeCell;

use crate::task::{TaskId, ThreadId};

mod bootstrap;
mod ipc;
mod preempt;
mod sleep;
mod thread;
mod transition;

#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) use self::preempt::{
    validate_invariants_for_test, wake_task_for_test, wake_thread_for_test,
};
pub(crate) use self::{
    bootstrap::{StaticTask, bootstrap},
    ipc::{
        WaitResult, block_current_on_ipc, clear_current_wait_result, complete_ipc_wait,
        set_current_wait_result, take_current_wait_result,
    },
    preempt::{enter_running_task, on_preempt_checkpoint},
    sleep::on_sleep_sync,
    thread::{
        block_current_on_process_wait, block_current_on_stdin_read, complete_process_wait,
        complete_stdin_read, current_user_address_space, current_user_process_id,
        on_thread_exit_sync, on_thread_join_sync, replace_current_user_address_space,
        thread_spawn_user, thread_spawn_user_from_context,
    },
};
pub use self::{
    preempt::{current_task_id, wake_task, yield_now},
    sleep::{msleep, sleep_until, sleep_until_counter, usleep},
    thread::{
        JoinError, SpawnError, ThreadArg, ThreadAttrs, ThreadEntry, current_thread_id, thread_exit,
        thread_join, thread_spawn,
    },
};

pub(crate) type Result<T> = core::result::Result<T, SchedError>;

const THREAD_STACK_SIZE: usize = 32768;
const IDLE_TASK_ID: TaskId = TaskId::new(0);
const INITIAL_THREAD_GENERATION: u32 = 1;

struct SchedulerCell(UnsafeCell<Option<Scheduler>>);

// SAFETY: genrt currently mutates scheduler state only on a single core.
unsafe impl Sync for SchedulerCell {}

static SCHEDULER: SchedulerCell = SchedulerCell(UnsafeCell::new(None));

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum SchedError {
    AlreadyBootstrapped,
    InvalidTaskId,
    ThreadCapacityTooSmall,
}

pub(crate) struct Scheduler {
    // Scheduler storage is dynamic-but-preallocated at bootstrap:
    // - `tasks` owns boxed stacks and inline SavedContext values; the task Vec
    //   is fully reserved and populated before first entry, so addresses remain
    //   stable while scheduling is active,
    // - `ready_queue` owns round-robin order for non-idle runnable tasks,
    // - no allocation or queue growth is allowed in IRQ fast paths.
    lifecycle: transition::TransitionState,
    rr_quantum_ms: u64,
    entered_running_task: bool,
}

#[inline(always)]
fn scheduler_slot_mut() -> &'static mut Option<Scheduler> {
    // SAFETY: Access is single-writer in the current single-core bring-up model.
    unsafe { &mut *SCHEDULER.0.get() }
}

#[inline(always)]
fn scheduler_mut() -> &'static mut Scheduler {
    scheduler_slot_mut()
        .as_mut()
        .unwrap_or_else(|| panic!("sched: scheduler is not initialized"))
}

#[inline(always)]
fn try_scheduler_mut() -> Option<&'static mut Scheduler> {
    scheduler_slot_mut().as_mut()
}

fn log_switch(prev: TaskId, next: TaskId) {
    crate::trace!("sched: prev={prev} next={next}");
}
