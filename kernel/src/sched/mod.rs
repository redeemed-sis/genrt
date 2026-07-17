use core::cell::UnsafeCell;

use core::fmt;

mod bootstrap;
pub mod call;
mod preempt;
mod sleep;
mod thread;
mod transition;
mod wait;

#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) use self::preempt::validate_invariants_for_test;
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) use self::wait::on_test_wait_sync;
pub(crate) use self::{
    bootstrap::{StaticThread, bootstrap},
    preempt::{enter_running_thread, on_preempt_checkpoint},
    sleep::on_sleep_sync,
    thread::{
        current_user_address_space, current_user_stack_ptr, on_thread_exit_sync,
        on_thread_join_sync, replace_current_user_resources, thread_spawn_user,
        thread_spawn_user_from_context,
    },
    wait::{
        CommitResult, CompletionResult, FinishError, PreparedWait, WaitCause, WaitToken,
        cancel_wait, commit_wait, complete_wait, finish_wait, prepare_wait,
    },
};
pub use self::{
    preempt::yield_now,
    sleep::{msleep, sleep_until, sleep_until_counter, usleep},
    thread::{
        JoinError, SpawnError, ThreadArg, ThreadAttrs, ThreadEntry, current_thread_id, thread_exit,
        thread_join, thread_spawn,
    },
};

pub(crate) type Result<T> = core::result::Result<T, SchedError>;
pub(crate) use transition::UserThreadResources;

const THREAD_STACK_SIZE: usize = 32768;
pub(super) const IDLE_THREAD_INDEX: usize = 0;
const INITIAL_THREAD_GENERATION: u32 = 1;

struct SchedulerCell(UnsafeCell<Option<Scheduler>>);

// SAFETY: genrt currently mutates scheduler state only on a single core.
unsafe impl Sync for SchedulerCell {}

static SCHEDULER: SchedulerCell = SchedulerCell(UnsafeCell::new(None));

/// Errors produced while constructing the bounded scheduler bootstrap state.
///
/// These values are copyable diagnostics only; creating or inspecting one does
/// not allocate, block, or alter IRQ state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum SchedError {
    /// Scheduler storage was already published.
    AlreadyBootstrapped,
    /// A lifecycle operation received an invalid or stale thread identity.
    InvalidThreadId,
    /// Configured capacity cannot hold idle and all bootstrap threads.
    ThreadCapacityTooSmall,
}

pub(crate) struct Scheduler {
    // Scheduler storage is dynamic-but-preallocated at bootstrap:
    // - `threads` owns boxed stacks and inline SavedContext values; the thread Vec
    //   is fully reserved and populated before first entry, so addresses remain
    //   stable while scheduling is active,
    // - `ready_queue` owns round-robin order for non-idle runnable threads,
    // - no allocation or queue growth is allowed in IRQ fast paths.
    lifecycle: transition::ThreadTable,
    rr_quantum_ms: u64,
    entered_running_thread: bool,
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

/// Generation-checked scheduler-thread handle.
///
/// The index directly names a bounded scheduler slot; the generation changes
/// before reuse, so stale IDs cannot address a later occupant.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ThreadId {
    index: usize,
    generation: u32,
}

impl ThreadId {
    /// Construct an ID for one bounded scheduler slot generation.
    ///
    /// # Arguments
    ///
    /// * `index` - Direct preallocated table index.
    /// * `generation` - Nonzero generation currently assigned to that slot.
    ///
    /// # Returns
    ///
    /// Returns a copyable handle. Scheduler lookup rejects it unless both
    /// fields still match a live occupant; construction allocates nothing and
    /// does not alter IRQ or scheduler state.
    pub(crate) const fn new(index: usize, generation: u32) -> Self {
        Self { index, generation }
    }

    /// Return the bounded scheduler slot index.
    ///
    /// # Returns
    ///
    /// Returns the direct table index without validating liveness. This is
    /// bounded and does not allocate, block, or alter IRQ state; callers must
    /// pair it with scheduler generation validation before dereferencing slots.
    pub const fn index(self) -> usize {
        self.index
    }

    /// Return the generation validated by scheduler lifecycle APIs.
    ///
    /// # Returns
    ///
    /// Returns the generation component without checking liveness. This is
    /// bounded and does not allocate, block, or alter IRQ state.
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.index, self.generation)
    }
}

impl fmt::Debug for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadId")
            .field("index", &self.index)
            .field("generation", &self.generation)
            .finish()
    }
}

fn log_switch(prev: ThreadId, next: ThreadId) {
    crate::trace!("sched: prev={prev} next={next}");
}
