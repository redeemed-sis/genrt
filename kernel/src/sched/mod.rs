use alloc::collections::VecDeque;
use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
};

use core::fmt;

use crate::{
    config::KERNEL_CPU_CAPACITY,
    cpu::{self, CpuId},
};

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
        JoinError, SpawnError, ThreadAffinity, ThreadArg, ThreadAttrs, ThreadEntry,
        current_thread_id, thread_exit, thread_join, thread_spawn,
    },
};

pub(crate) type Result<T> = core::result::Result<T, SchedError>;
pub(crate) use transition::UserThreadResources;

const THREAD_STACK_SIZE: usize = 32768;
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
    // Each element owns execution-local scheduling state for one logical CPU.
    // The vector is fully allocated and populated at scheduler bootstrap; no
    // transition, IRQ path, or handoff may grow it or a local ready queue.
    cpus: [CpuSchedulerState; KERNEL_CPU_CAPACITY],
    rr_quantum_ms: u64,
}

/// Mutable scheduler view bound to one already-resolved logical CPU.
///
/// Current-CPU entry points construct this view once before a scheduler
/// operation. Local transitions then use the stable bound identity without
/// repeating architecture lookup or threading `CpuId` through every helper.
/// Explicit target/home-CPU operations may create a separate view for that
/// CPU, keeping remote ownership visible at the call site.
pub(super) struct CpuScheduler<'a> {
    scheduler: &'a mut Scheduler,
    cpu: CpuId,
}

impl Scheduler {
    /// Bind a mutable scheduler view to one checked logical CPU.
    ///
    /// # Arguments
    ///
    /// * `cpu` - Logical CPU whose local scheduler context the returned view
    ///   operates on.
    ///
    /// # Returns
    ///
    /// Returns a non-allocating mutable view that borrows the scheduler and
    /// retains `cpu` for the borrow lifetime. Invalid fixed-storage indexes
    /// panic when the view first accesses local state.
    pub(super) fn on_cpu(&mut self, cpu: CpuId) -> CpuScheduler<'_> {
        CpuScheduler {
            scheduler: self,
            cpu,
        }
    }

    fn cpu_state_for(&self, cpu: CpuId) -> &CpuSchedulerState {
        self.cpus
            .get(cpu.index())
            .unwrap_or_else(|| panic!("sched: invalid CPU{} state lookup", cpu.index()))
    }

    fn cpu_state_for_mut(&mut self, cpu: CpuId) -> &mut CpuSchedulerState {
        self.cpus
            .get_mut(cpu.index())
            .unwrap_or_else(|| panic!("sched: invalid CPU{} state lookup", cpu.index()))
    }
}

impl CpuScheduler<'_> {
    /// Return the logical CPU bound to this scheduler view.
    ///
    /// # Returns
    ///
    /// Returns the stable logical identity selected when the view was created.
    /// This copy-only operation does not allocate, block, or alter IRQ state.
    #[inline(always)]
    pub(super) const fn cpu(&self) -> CpuId {
        self.cpu
    }

    #[inline(always)]
    fn state(&self) -> &CpuSchedulerState {
        self.scheduler.cpu_state_for(self.cpu)
    }

    #[inline(always)]
    fn state_mut(&mut self) -> &mut CpuSchedulerState {
        self.scheduler.cpu_state_for_mut(self.cpu)
    }

    fn for_cpu(&mut self, cpu: CpuId) -> CpuScheduler<'_> {
        self.scheduler.on_cpu(cpu)
    }
}

impl Deref for CpuScheduler<'_> {
    type Target = Scheduler;

    fn deref(&self) -> &Self::Target {
        self.scheduler
    }
}

impl DerefMut for CpuScheduler<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.scheduler
    }
}

/// Bounded scheduling state owned by one logical CPU.
///
/// The embedded preemption state's immutable owner matches this context's
/// array index. A guard and its scheduler checkpoint therefore address the
/// same CPU-local runtime state after scheduler publication.
struct CpuSchedulerState {
    current: Option<ThreadId>,
    idle: Option<ThreadId>,
    ready_queue: VecDeque<ThreadId>,
    initialized: bool,
    online: bool,
    preemption: crate::sync::preempt::PreemptionState,
}

impl CpuSchedulerState {
    fn new(cpu: CpuId, thread_capacity: usize) -> Self {
        let mut ready_queue = VecDeque::new();
        ready_queue.reserve_exact(thread_capacity.saturating_sub(1));
        Self {
            current: None,
            idle: None,
            ready_queue,
            initialized: false,
            online: false,
            preemption: crate::sync::preempt::PreemptionState::new(cpu),
        }
    }
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

/// Borrow one published CPU context's preemption state.
///
/// This internal bridge lets pre-scheduler synchronization code switch from
/// fixed boot backing to scheduler-owned runtime backing after publication.
/// Callers must hold local IRQ exclusion and must not already borrow the
/// scheduler. The lookup is bounded and allocation-free.
///
/// # Arguments
///
/// * `cpu` - Logical CPU whose runtime preemption state is requested.
///
/// # Returns
///
/// Returns `Some` after scheduler publication, or `None` while bootstrap still
/// uses the fixed pre-scheduler backing.
pub(crate) fn runtime_preemption_state_mut(
    cpu: CpuId,
) -> Option<&'static mut crate::sync::preempt::PreemptionState> {
    try_scheduler_mut().map(|scheduler| &mut scheduler.cpu_state_for_mut(cpu).preemption)
}

#[inline(always)]
pub(super) fn current_cpu() -> CpuId {
    cpu::current_id().unwrap_or_else(|err| panic!("sched: current CPU lookup failed: {err:?}"))
}

/// Return whether the registered `cpu` has entered a running scheduler context.
///
/// This is a bounded, allocation-free query used by CPU-local preemption
/// control. It takes short local IRQ exclusion and returns `false` before
/// scheduler publication or first entry.
///
/// # Arguments
///
/// * `cpu` - Registered logical CPU whose runtime state is queried.
///
/// # Returns
///
/// Returns `true` only after the selected CPU entered its first running
/// context. The query does not allocate or block.
pub(crate) fn scheduler_online(cpu: CpuId) -> bool {
    let _irq_guard = crate::sync::LocalIrqGuard::save_and_disable();
    try_scheduler_mut()
        .and_then(|scheduler| scheduler.cpus.get(cpu.index()))
        .is_some_and(|state| state.online)
}

/// Validate the active CPU's identity and scheduler-local ownership for QEMU.
///
/// This test-only seam performs a bounded, allocation-free walk under the
/// local IRQ guard it acquires. It does not enter production artifacts or
/// expose a runtime affinity API.
///
/// # Returns
///
/// Returns after validating the current CPU context and every thread's home
/// ownership.
///
/// # Panics
///
/// Panics when current, idle, ready, online, affinity, or secondary-context
/// state violates the CPU0-only scheduler contract.
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) fn validate_cpu_context_for_test() {
    let cpu = current_cpu();
    let _irq_guard = crate::sync::LocalIrqGuard::save_and_disable();
    scheduler_mut().validate_cpu_context_for_test(cpu);
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

fn log_switch(cpu: CpuId, prev: ThreadId, next: ThreadId) {
    crate::trace!("sched: cpu={} prev={prev} next={next}", cpu.index());
}
