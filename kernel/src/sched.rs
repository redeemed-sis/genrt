use alloc::{boxed::Box, collections::VecDeque, vec::Vec};
use core::{cell::UnsafeCell, mem};

use crate::{
    arch_consts::TASK_FRAME_WORDS,
    ipc::{IpcWaitRegistration, IpcWaitToken},
    task::TaskId,
    time::{TimeHandlers, TimedEvent},
};

pub(crate) type TaskEntry = fn() -> !;
pub(crate) type Result<T> = core::result::Result<T, SchedError>;

const TASK_STACK_SIZE: usize = 4096;
const IDLE_TASK_ID: TaskId = TaskId::new(0);

unsafe extern "C" {
    fn arch_init_task_frame(
        frame_words: *mut u64,
        stack_top: usize,
        entry_addr: usize,
        bootstrap_pc: usize,
    );
    fn arch_enter_task_frame(frame_words: *const u64) -> !;
}

struct SchedulerCell(UnsafeCell<Option<Scheduler>>);

// SAFETY: genrt currently mutates scheduler state only on a single core.
unsafe impl Sync for SchedulerCell {}

static SCHEDULER: SchedulerCell = SchedulerCell(UnsafeCell::new(None));

#[derive(Copy, Clone)]
pub(crate) struct StaticTask {
    priority: u8,
    entry: TaskEntry,
}

impl StaticTask {
    pub(crate) const fn new(priority: u8, entry: TaskEntry) -> Self {
        Self { priority, entry }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum WaitResult {
    Completed,
    TimedOut,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct IpcBlock {
    token: IpcWaitToken,
    timeout_event: Option<TimedEvent>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BlockReason {
    Sleep,
    Ipc(IpcBlock),
}

// Task-state semantics:
// - Running: the sole task whose saved frame matches the scheduler's committed resume target.
// - Ready: runnable, owns a valid saved frame, and sits in the ready queue unless it is idle.
// - Blocked(reason): not runnable and not considered during round-robin
//   selection. Wakeup ownership still lives outside the scheduler: deadlines in
//   `kernel::time` and wait queues in `kernel::ipc`. The scheduler keeps an
//   opaque IPC wait token so timeout dispatch can ask IPC to remove the waiter
//   before restoring the task to `Ready`, without knowing the concrete IPC type.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TaskState {
    Ready,
    Running,
    Blocked(BlockReason),
}

#[repr(C, align(16))]
struct TaskStack {
    bytes: [u8; TASK_STACK_SIZE],
}

impl TaskStack {
    const fn zeroed() -> Self {
        Self {
            bytes: [0; TASK_STACK_SIZE],
        }
    }

    fn top(&self) -> usize {
        self.bytes.as_ptr() as usize + TASK_STACK_SIZE
    }
}

#[repr(C, align(16))]
struct TaskFrameStorage {
    words: [u64; TASK_FRAME_WORDS],
}

impl TaskFrameStorage {
    const fn zeroed() -> Self {
        Self {
            words: [0; TASK_FRAME_WORDS],
        }
    }

    fn as_words_ptr(&self) -> *const u64 {
        self.words.as_ptr()
    }

    fn as_mut_words_ptr(&mut self) -> *mut u64 {
        self.words.as_mut_ptr()
    }
}

struct Task {
    state: TaskState,
    last_wait_result: Option<WaitResult>,
    entry: TaskEntry,
    stack: Box<TaskStack>,
    frame: Box<TaskFrameStorage>,
}

impl Task {
    fn new(entry: TaskEntry, state: TaskState) -> Self {
        Self {
            state,
            last_wait_result: None,
            entry,
            stack: Box::new(TaskStack::zeroed()),
            frame: Box::new(TaskFrameStorage::zeroed()),
        }
    }

    fn stack_top(&self) -> usize {
        self.stack.top()
    }

    fn is_blocked(&self) -> bool {
        matches!(self.state, TaskState::Blocked(_))
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum SchedError {
    AlreadyBootstrapped,
    InvalidTaskId,
}

pub(crate) struct Scheduler {
    // Scheduler storage is now dynamic-but-preallocated at bootstrap:
    // - `tasks` owns boxed stacks and saved frames for stable addresses,
    // - `ready_queue` owns round-robin order for non-idle runnable tasks,
    // - no allocation or queue growth is allowed in IRQ fast paths.
    tasks: Vec<Task>,
    ready_queue: VecDeque<TaskId>,
    current: Option<TaskId>,
    rr_quantum_ms: u64,
    // Set when `kernel::time` dispatches `QuantumExpired(current_task)`.
    // The actual switch decision is still committed only in the scheduler's
    // frame-handoff path.
    resched_requested: bool,
    entered_running_task: bool,
}

pub(crate) fn bootstrap(
    idle_entry: TaskEntry,
    tasks: &[StaticTask],
    rr_quantum_ms: u64,
) -> Result<()> {
    if scheduler_slot_mut().is_some() {
        return Err(SchedError::AlreadyBootstrapped);
    }

    // Bootstrap ordering is intentionally split into two phases:
    // 1. Build and publish the global scheduler so timer-owned callbacks always
    //    have a concrete scheduler instance to reach.
    // 2. Only then initialize `kernel::time`, which is the first point where
    //    timer IRQ dispatch can invoke scheduler callbacks.
    let scheduler = Scheduler::bootstrap_new(idle_entry, tasks, rr_quantum_ms)?;
    let task_count = scheduler.tasks.len();
    *scheduler_slot_mut() = Some(scheduler);
    init_time_after_scheduler_publish(task_count);
    Ok(())
}

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

fn on_wake_task(task_id: TaskId) {
    wake_task(task_id);
}

fn on_quantum_expired(task_id: TaskId) {
    scheduler_mut().note_quantum_expired(task_id);
}

fn on_ipc_timeout(task_id: TaskId) {
    scheduler_mut().handle_ipc_timeout(task_id);
}

#[inline(always)]
fn init_time_after_scheduler_publish(task_count: usize) {
    crate::time::init(
        TimeHandlers {
            finish_timer_interrupt,
            wake_task: on_wake_task,
            quantum_expired: on_quantum_expired,
            ipc_timeout: on_ipc_timeout,
        },
        task_count.saturating_mul(3),
    );
}

fn finish_timer_interrupt(active_frame_words: *mut u64, now: u64) {
    if active_frame_words.is_null() {
        return;
    }

    scheduler_mut().finish_timer_interrupt(active_frame_words, now);
}

pub fn on_sleep_sync(active_frame_words: *mut u64, deadline: u64) {
    scheduler_mut().block_current_until(active_frame_words, deadline);
}

pub fn current_task_id() -> Option<TaskId> {
    scheduler_mut().running_task()
}

pub fn wake_task(task_id: TaskId) {
    scheduler_mut().wake_task(task_id);
}

pub(crate) fn complete_ipc_wait(task_id: TaskId) {
    scheduler_mut().complete_ipc_wait(task_id);
}

pub(crate) fn clear_current_wait_result() {
    scheduler_mut().clear_current_wait_result();
}

pub(crate) fn set_current_wait_result(result: WaitResult) {
    scheduler_mut().set_current_wait_result(result);
}

pub(crate) fn take_current_wait_result() -> Option<WaitResult> {
    scheduler_mut().take_current_wait_result()
}

pub(crate) fn block_current_on_ipc(active_frame_words: *mut u64, wait: IpcWaitRegistration) {
    scheduler_mut().block_current_on_ipc(active_frame_words, wait);
}

pub(crate) fn enter_running_task() -> ! {
    scheduler_mut().enter_running_task()
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

impl Scheduler {
    fn bootstrap_new(
        idle_entry: TaskEntry,
        tasks: &[StaticTask],
        rr_quantum_ms: u64,
    ) -> Result<Self> {
        let task_count = tasks.len() + 1;
        let mut scheduler = Self::new(task_count, rr_quantum_ms);
        scheduler.push_idle_task(idle_entry);
        for task in tasks {
            scheduler.push_bootstrap_task(task.priority, task.entry);
        }
        scheduler.initialize_all_task_frames()?;

        let first = scheduler.dequeue_next_ready().unwrap_or(IDLE_TASK_ID);
        scheduler.mark_initial_running(first)?;
        crate::debug!(
            "sched: task_count={} ready_queue_capacity={} quantum={}ms",
            scheduler.tasks.len(),
            scheduler.ready_queue.capacity(),
            scheduler.rr_quantum_ms
        );
        Ok(scheduler)
    }

    fn new(task_capacity: usize, rr_quantum_ms: u64) -> Self {
        let mut task_table = Vec::new();
        task_table.reserve_exact(task_capacity);

        let mut ready_queue = VecDeque::new();
        ready_queue.reserve_exact(task_capacity.saturating_sub(1));

        Self {
            tasks: task_table,
            ready_queue,
            current: None,
            rr_quantum_ms: rr_quantum_ms.max(1),
            resched_requested: false,
            entered_running_task: false,
        }
    }

    fn push_idle_task(&mut self, entry: TaskEntry) {
        let id = TaskId::new(self.tasks.len());
        debug_assert_eq!(id, IDLE_TASK_ID);
        self.tasks.push(Task::new(entry, TaskState::Ready));
    }

    fn push_bootstrap_task(&mut self, priority: u8, entry: TaskEntry) {
        let id = TaskId::new(self.tasks.len());
        self.tasks.push(Task::new(entry, TaskState::Ready));
        self.ready_push_back(id);
        crate::debug!("sched: bootstrap task {} priority={priority}", id);
    }

    fn initialize_all_task_frames(&mut self) -> Result<()> {
        for index in 0..self.tasks.len() {
            self.initialize_task_frame(TaskId::new(index))?;
        }
        Ok(())
    }

    fn initialize_task_frame(&mut self, id: TaskId) -> Result<()> {
        if !self.is_valid_task(id) {
            return Err(SchedError::InvalidTaskId);
        }

        let task = self.task_mut(id);
        // SAFETY: scheduler owns frame storage, stack top and entry pointer are
        // validated by bootstrap task-table setup.
        unsafe {
            arch_init_task_frame(
                task.frame.as_mut_words_ptr(),
                task.stack_top(),
                task.entry as usize,
                task_entry_bootstrap as usize,
            );
        }
        Ok(())
    }

    fn finish_timer_interrupt(&mut self, active_frame_words: *mut u64, now: u64) {
        if !self.entered_running_task {
            return;
        }

        // Scheduler IRQ-return handoff policy: this path must remain heap-free.
        // The kernel heap is IRQ-safe for task-context allocation, but the
        // timer/scheduler fast path must continue to run on preallocated state.
        let current = match self.running_task() {
            Some(id) => id,
            None => return,
        };
        let must_leave_idle = current == IDLE_TASK_ID && !self.ready_queue.is_empty();
        let mut refreshed_quantum_event = false;

        if self.resched_requested || must_leave_idle {
            let next = self.dequeue_next_ready().unwrap_or(current);
            if next != current {
                copy_words(
                    self.frame_words_mut_ptr(current),
                    active_frame_words as *const u64,
                );
                copy_words(active_frame_words, self.frame_words_ptr(next));
                self.commit_switch(current, next);
                log_switch(current, next);
            }

            self.replace_quantum_event(now, current);
            refreshed_quantum_event = true;
        }

        if !refreshed_quantum_event {
            self.ensure_quantum_event(now);
        }

        self.resched_requested = false;
    }

    fn block_current_until(&mut self, active_frame_words: *mut u64, deadline: u64) {
        let (current, next) = self.begin_block_current(active_frame_words, BlockReason::Sleep);
        crate::time::schedule_event(deadline, TimedEvent::WakeTask(current));
        self.finish_block_current(current, next);
        crate::trace!("sched: task {current} sleeping until counter {deadline}");
    }

    fn block_current_on_ipc(&mut self, active_frame_words: *mut u64, wait: IpcWaitRegistration) {
        let current = self.blocking_current(active_frame_words);
        let timeout_event = wait.timeout_deadline().map(|deadline| {
            let event = TimedEvent::IpcTimeout(current);
            crate::time::schedule_event(deadline, event);
            crate::debug!("sched: timeout event scheduled {event:?} deadline={deadline}");
            event
        });
        let reason = BlockReason::Ipc(IpcBlock {
            token: wait.token(),
            timeout_event,
        });

        let next = self.block_current_with_reason(active_frame_words, current, reason);
        self.finish_block_current(current, next);
        crate::trace!(
            "sched: task {current} blocked on IPC token={:?} timeout_event={:?}",
            wait.token(),
            timeout_event
        );
    }

    fn begin_block_current(
        &mut self,
        active_frame_words: *mut u64,
        reason: BlockReason,
    ) -> (TaskId, TaskId) {
        let current = self.blocking_current(active_frame_words);
        let next = self.block_current_with_reason(active_frame_words, current, reason);
        (current, next)
    }

    fn blocking_current(&self, active_frame_words: *mut u64) -> TaskId {
        if active_frame_words.is_null() {
            panic!("sched: block without active frame");
        }

        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("block requested without a running task"));
        if current == IDLE_TASK_ID {
            panic!("sched: idle task cannot block");
        }

        current
    }

    fn block_current_with_reason(
        &mut self,
        active_frame_words: *mut u64,
        current: TaskId,
        reason: BlockReason,
    ) -> TaskId {
        let next = self.dequeue_next_ready().unwrap_or(IDLE_TASK_ID);

        // Save the current task's post-SVC resume frame before making it unrunnable.
        copy_words(
            self.frame_words_mut_ptr(current),
            active_frame_words as *const u64,
        );

        copy_words(active_frame_words, self.frame_words_ptr(next));
        self.commit_block(current, next, reason);
        next
    }

    fn finish_block_current(&mut self, current: TaskId, next: TaskId) {
        let now = crate::time::now_counter();
        self.replace_quantum_event(now, current);
        if next != current {
            log_switch(current, next);
        }
    }

    fn enter_running_task(&mut self) -> ! {
        let running = self
            .running_task()
            .unwrap_or_else(|| panic!("scheduler has no running task"));
        let frame_words = self.frame_words_ptr(running);
        let now = crate::time::now_counter();

        self.entered_running_task = true;
        self.ensure_quantum_event(now);
        debug_assert_eq!(self.current, Some(running));
        debug_assert_eq!(self.task(running).state, TaskState::Running);
        // SAFETY: scheduler bootstrap prepared the running task trap frame.
        unsafe { arch_enter_task_frame(frame_words) }
    }
}

impl Scheduler {
    fn running_task(&self) -> Option<TaskId> {
        self.current
    }

    fn mark_initial_running(&mut self, id: TaskId) -> Result<()> {
        if !self.is_valid_task(id) {
            return Err(SchedError::InvalidTaskId);
        }

        if self.task(id).is_blocked() {
            return Err(SchedError::InvalidTaskId);
        }

        self.make_running(id);
        debug_assert_eq!(self.task(id).state, TaskState::Running);
        debug_assert_eq!(self.current, Some(id));
        Ok(())
    }

    fn commit_switch(&mut self, prev: TaskId, next: TaskId) {
        if prev == next {
            return;
        }

        debug_assert_eq!(self.current, Some(prev));
        debug_assert_eq!(self.task(prev).state, TaskState::Running);
        debug_assert!(!self.task(next).is_blocked());

        self.make_ready(prev);
        if prev != IDLE_TASK_ID {
            self.ready_push_back(prev);
        }
        self.make_running(next);
        debug_assert_eq!(self.current, Some(next));
        debug_assert_eq!(self.task(next).state, TaskState::Running);
        debug_assert_eq!(self.task(prev).state, TaskState::Ready);
    }

    fn commit_block(&mut self, blocked: TaskId, next: TaskId, reason: BlockReason) {
        debug_assert_eq!(self.current, Some(blocked));
        debug_assert_eq!(self.task(blocked).state, TaskState::Running);
        debug_assert!(!self.task(next).is_blocked());

        self.make_blocked(blocked, reason);
        self.make_running(next);

        debug_assert_eq!(self.current, Some(next));
        debug_assert!(self.task(blocked).is_blocked());
        debug_assert_eq!(self.task(next).state, TaskState::Running);
    }
}

impl Scheduler {
    fn wake_task(&mut self, task_id: TaskId) {
        if !self.is_valid_task(task_id) {
            return;
        }

        if self.task(task_id).is_blocked() {
            self.make_ready(task_id);
            if task_id != IDLE_TASK_ID {
                self.ready_push_back(task_id);
            }
            crate::trace!("sched: task {task_id} moved to Ready");
        }
    }

    fn complete_ipc_wait(&mut self, task_id: TaskId) {
        if !self.is_valid_task(task_id) {
            return;
        }

        let timeout_event = match self.task(task_id).state {
            TaskState::Blocked(BlockReason::Ipc(wait)) => wait.timeout_event,
            TaskState::Blocked(reason) => {
                crate::trace!(
                    "sched: ignoring IPC completion for task {task_id}; blocked on {reason:?}"
                );
                return;
            }
            state => {
                crate::trace!("sched: ignoring IPC completion for task {task_id}; state={state:?}");
                return;
            }
        };

        if let Some(event) = timeout_event {
            crate::time::cancel_event(event);
            crate::debug!("sched: normal IPC wake canceled timeout {event:?}");
        }

        self.task_mut(task_id).last_wait_result = Some(WaitResult::Completed);
        self.wake_task(task_id);
    }

    fn handle_ipc_timeout(&mut self, task_id: TaskId) {
        if !self.is_valid_task(task_id) {
            return;
        }

        let wait = match self.task(task_id).state {
            TaskState::Blocked(BlockReason::Ipc(wait))
                if wait.timeout_event == Some(TimedEvent::IpcTimeout(task_id)) =>
            {
                wait
            }
            TaskState::Blocked(BlockReason::Ipc(wait)) => {
                crate::trace!("sched: stale IPC timeout for task {task_id}; current wait={wait:?}");
                return;
            }
            state => {
                crate::trace!("sched: stale IPC timeout for task {task_id}; state={state:?}");
                return;
            }
        };

        let removed = crate::ipc::remove_timed_out_waiter(wait.token, task_id);
        if !removed {
            panic!("sched: IPC timeout task {task_id} missing from IPC wait queue");
        }

        self.task_mut(task_id).last_wait_result = Some(WaitResult::TimedOut);
        self.wake_task(task_id);
        crate::debug!(
            "sched: IPC timeout completed task {task_id} token={:?}",
            wait.token
        );
    }

    fn clear_current_wait_result(&mut self) {
        if let Some(current) = self.current {
            self.task_mut(current).last_wait_result = None;
        }
    }

    fn set_current_wait_result(&mut self, result: WaitResult) {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("sched: wait result without running task"));
        self.task_mut(current).last_wait_result = Some(result);
    }

    fn take_current_wait_result(&mut self) -> Option<WaitResult> {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("sched: wait result without running task"));
        self.task_mut(current).last_wait_result.take()
    }

    fn note_quantum_expired(&mut self, task_id: TaskId) {
        if self.current == Some(task_id) {
            self.resched_requested = true;
            crate::trace!("sched: quantum expired task {task_id}");
        }
    }

    fn ready_push_back(&mut self, id: TaskId) {
        debug_assert_ne!(id, IDLE_TASK_ID);
        debug_assert!(
            !self.ready_queue.iter().any(|queued| *queued == id),
            "sched: task already present in ready queue"
        );

        if self.ready_queue.len() == self.ready_queue.capacity() {
            panic!("sched: ready queue capacity exhausted");
        }

        self.ready_queue.push_back(id);
    }

    fn dequeue_next_ready(&mut self) -> Option<TaskId> {
        self.ready_queue.pop_front()
    }

    fn make_ready(&mut self, id: TaskId) {
        self.task_mut(id).state = TaskState::Ready;
    }

    fn make_running(&mut self, id: TaskId) {
        self.task_mut(id).state = TaskState::Running;
        self.current = Some(id);
    }

    fn make_blocked(&mut self, id: TaskId, reason: BlockReason) {
        self.task_mut(id).state = TaskState::Blocked(reason);
    }

    fn has_runnable_peer(&self) -> bool {
        !self.ready_queue.is_empty()
    }

    fn ensure_quantum_event(&mut self, now: u64) {
        let Some(current) = self.current else {
            return;
        };

        let event = TimedEvent::QuantumExpired(current);
        if !self.has_runnable_peer() {
            crate::time::cancel_event(event);
            return;
        }

        if crate::time::event_pending(event) {
            return;
        }

        let deadline = now.wrapping_add(crate::time::ms_to_counts(self.rr_quantum_ms));
        crate::time::schedule_event(deadline, event);
        crate::trace!(
            "sched: task {current} quantum={}ms until counter {deadline}",
            self.rr_quantum_ms
        );
    }

    fn replace_quantum_event(&mut self, now: u64, obsolete_task: TaskId) {
        crate::time::cancel_event(TimedEvent::QuantumExpired(obsolete_task));
        self.ensure_quantum_event(now);
    }

    fn is_valid_task(&self, id: TaskId) -> bool {
        id.index() < self.tasks.len()
    }

    fn task(&self, id: TaskId) -> &Task {
        &self.tasks[id.index()]
    }

    fn task_mut(&mut self, id: TaskId) -> &mut Task {
        &mut self.tasks[id.index()]
    }

    fn frame_words_ptr(&self, id: TaskId) -> *const u64 {
        self.task(id).frame.as_words_ptr()
    }

    fn frame_words_mut_ptr(&mut self, id: TaskId) -> *mut u64 {
        self.task_mut(id).frame.as_mut_words_ptr()
    }
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

extern "C" fn task_entry_bootstrap(entry_addr: usize) -> ! {
    // SAFETY: task frame setup passes `TaskEntry` function pointers in x0.
    let entry_fn: TaskEntry = unsafe { mem::transmute(entry_addr) };
    entry_fn();

    #[allow(unreachable_code)]
    {
        panic!("task entry returned unexpectedly");
    }
}

fn log_switch(prev: TaskId, next: TaskId) {
    crate::trace!("sched: prev={prev} next={next}");
}
