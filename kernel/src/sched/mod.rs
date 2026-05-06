use alloc::{collections::VecDeque, vec::Vec};
use core::cell::UnsafeCell;

use crate::{arch_consts::TASK_FRAME_WORDS, task::TaskId};

mod bootstrap;
mod ipc;
mod preempt;
mod sleep;
mod thread;

pub(crate) use self::{
    bootstrap::{StaticTask, bootstrap},
    ipc::{
        WaitResult, block_current_on_ipc, clear_current_wait_result, complete_ipc_wait,
        set_current_wait_result, take_current_wait_result,
    },
    preempt::enter_running_task,
    sleep::on_sleep_sync,
    thread::{on_thread_exit_sync, on_thread_join_sync},
};
pub use self::{
    preempt::{current_task_id, wake_task},
    sleep::{msleep, sleep_until, sleep_until_counter, usleep},
    thread::{
        JoinError, SpawnError, ThreadArg, ThreadAttrs, ThreadEntry, current_thread_id, thread_exit,
        thread_join, thread_spawn,
    },
};

pub(crate) type Result<T> = core::result::Result<T, SchedError>;

const THREAD_STACK_SIZE: usize = 8192;
const IDLE_TASK_ID: TaskId = TaskId::new(0);
const INITIAL_THREAD_GENERATION: u32 = 1;

unsafe extern "C" {
    fn arch_init_thread_frame(
        frame_words: *mut u64,
        stack_top: usize,
        entry_addr: usize,
        arg: usize,
        bootstrap_pc: usize,
    );
    fn arch_enter_task_frame(frame_words: *const u64) -> !;
}

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
    // - `tasks` owns boxed stacks and saved frames for stable addresses,
    // - `ready_queue` owns round-robin order for non-idle runnable tasks,
    // - no allocation or queue growth is allowed in IRQ fast paths.
    tasks: Vec<preempt::Task>,
    ready_queue: VecDeque<TaskId>,
    current: Option<TaskId>,
    rr_quantum_ms: u64,
    // Set when `kernel::time` dispatches `QuantumExpired(current_task)`.
    // The actual switch decision is still committed only in the scheduler's
    // frame-handoff path.
    resched_requested: bool,
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

#[inline(always)]
fn copy_words(dst: *mut u64, src: *const u64) {
    // SAFETY: caller guarantees both buffers are valid and non-overlapping
    // `TASK_FRAME_WORDS` storage.
    unsafe {
        for i in 0..TASK_FRAME_WORDS {
            *dst.add(i) = *src.add(i);
        }
    }
}

fn log_switch(prev: TaskId, next: TaskId) {
    crate::trace!("sched: prev={prev} next={next}");
}
