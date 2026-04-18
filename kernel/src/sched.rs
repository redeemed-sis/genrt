use core::{cell::UnsafeCell, mem};

use crate::{
    arch_consts::TASK_FRAME_WORDS,
    time::{TimeHandlers, TimedEvent, TimedTaskId},
};

pub(crate) type TaskId = usize;
pub(crate) type TaskEntry = fn() -> !;
pub(crate) type Result<T> = core::result::Result<T, SchedError>;

pub(crate) const MAX_TASKS: usize = 8;
const TASK_STACK_SIZE: usize = 4096;
const IDLE_TASK_ID: TaskId = 0;
const IDLE_PRIORITY: u8 = 0;

unsafe extern "C" {
    fn arch_init_task_frame(
        frame_words: *mut u64,
        stack_top: usize,
        entry_addr: usize,
        bootstrap_pc: usize,
    );
    fn arch_enter_task_frame(frame_words: *const u64) -> !;
    fn arch_sleep_until(deadline: u64);
}

struct SchedulerCell(UnsafeCell<Scheduler>);

// SAFETY: genrt currently mutates scheduler state only on a single core.
unsafe impl Sync for SchedulerCell {}

static SCHEDULER: SchedulerCell = SchedulerCell(UnsafeCell::new(Scheduler::new()));

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

// Task-state semantics:
// - Running: the sole task whose saved frame matches the scheduler's committed resume target.
// - Ready: runnable and owns a valid saved frame, but is not the current resume target.
// - Blocked: not runnable and not considered during round-robin selection.
//   Wakeup deadlines live in `kernel::time`; the scheduler only observes the
//   resulting `WakeTask(task_id)` timed event and restores the task to `Ready`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TaskState {
    Ready,
    Running,
    Blocked,
}

#[repr(C, align(16))]
#[derive(Copy, Clone)]
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
#[derive(Copy, Clone)]
struct TaskFrameStorage {
    words: [u64; TASK_FRAME_WORDS],
}

impl TaskFrameStorage {
    const fn zeroed() -> Self {
        Self {
            words: [0; TASK_FRAME_WORDS],
        }
    }

    fn clear(&mut self) {
        for word in &mut self.words {
            *word = 0;
        }
    }

    fn as_words_ptr(&self) -> *const u64 {
        self.words.as_ptr()
    }

    fn as_mut_words_ptr(&mut self) -> *mut u64 {
        self.words.as_mut_ptr()
    }
}

#[derive(Copy, Clone)]
struct Task {
    id: TaskId,
    priority: u8,
    state: TaskState,
    entry: TaskEntry,
    stack_top: usize,
    frame: TaskFrameStorage,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum SchedError {
    InvalidTaskId,
    AlreadyScheduled,
    NoFreeTaskSlot,
}

pub(crate) struct Scheduler {
    tasks: [Task; MAX_TASKS],
    stacks: [TaskStack; MAX_TASKS],
    // `current` is the scheduler's committed CPU-resume target. Once task execution starts,
    // it must match the frame that IRQ return or `arch_enter_task_frame()` will resume.
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
    let sched = scheduler_mut();
    sched.bootstrap(idle_entry, tasks, rr_quantum_ms)
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

    // SAFETY: `arch_sleep_until()` enters a controlled synchronous exception path that
    // saves the current task frame and hands ownership back to the scheduler.
    // The deadline is expressed in hardware counter units.
    unsafe { arch_sleep_until(deadline) }
}

#[inline(always)]
pub fn sleep_until(deadline: u64) {
    sleep_until_counter(deadline);
}

fn on_wake_task(task_id: TimedTaskId) {
    scheduler_mut().wake_task(task_id);
}

fn on_quantum_expired(task_id: TimedTaskId) {
    scheduler_mut().note_quantum_expired(task_id);
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

pub(crate) fn enter_running_task() -> ! {
    scheduler_mut().enter_running_task()
}

#[inline(always)]
fn scheduler_mut() -> &'static mut Scheduler {
    // SAFETY: Access is single-writer in the current single-core bring-up model.
    unsafe { &mut *SCHEDULER.0.get() }
}

impl Scheduler {
    pub(crate) const fn new() -> Self {
        Self {
            tasks: [
                Task::blocked(0),
                Task::blocked(1),
                Task::blocked(2),
                Task::blocked(3),
                Task::blocked(4),
                Task::blocked(5),
                Task::blocked(6),
                Task::blocked(7),
            ],
            stacks: [
                TaskStack::zeroed(),
                TaskStack::zeroed(),
                TaskStack::zeroed(),
                TaskStack::zeroed(),
                TaskStack::zeroed(),
                TaskStack::zeroed(),
                TaskStack::zeroed(),
                TaskStack::zeroed(),
            ],
            current: None,
            rr_quantum_ms: 10,
            resched_requested: false,
            entered_running_task: false,
        }
    }
}

impl Scheduler {
    fn bootstrap(
        &mut self,
        idle_entry: TaskEntry,
        tasks: &[StaticTask],
        rr_quantum_ms: u64,
    ) -> Result<()> {
        crate::time::init(TimeHandlers {
            finish_timer_interrupt,
            wake_task: on_wake_task,
            quantum_expired: on_quantum_expired,
        });
        self.init(rr_quantum_ms);
        self.set_idle_task(idle_entry);
        self.initialize_task_frame(IDLE_TASK_ID)?;
        for task in tasks {
            let id = self.add_ready_task(task.priority, task.entry)?;
            self.initialize_task_frame(id)?;
        }
        let first = self
            .pick_next_excluding(IDLE_TASK_ID, None)
            .unwrap_or(IDLE_TASK_ID);
        self.mark_initial_running(first)?;
        Ok(())
    }

    fn finish_timer_interrupt(&mut self, active_frame_words: *mut u64, now: u64) {
        if !self.entered_running_task {
            return;
        }

        let current = match self.running_task() {
            Some(id) => id,
            None => return,
        };
        let must_leave_idle = current == IDLE_TASK_ID && self.has_runnable_peer(current);
        let mut replaced_quantum_event = false;

        if self.resched_requested || must_leave_idle {
            let next = self.select_next();
            if next != current {
                // Switch-commit invariant:
                // 1. persist the interrupted running task frame
                // 2. install the selected next frame into the live IRQ-return slot
                // 3. only then update logical task-state bookkeeping
                //
                // This keeps the scheduler's `Running` state aligned with the actual
                // CPU resume target at the handoff point.
                copy_words(
                    self.tasks[current].frame.as_mut_words_ptr(),
                    active_frame_words as *const u64,
                );
                copy_words(active_frame_words, self.tasks[next].frame.as_words_ptr());
                self.commit_switch(current, next);
                log_switch(current, next);
            }

            self.replace_quantum_event(now, current);
            replaced_quantum_event = true;
        }

        if !replaced_quantum_event {
            self.ensure_quantum_event(now);
        }

        self.resched_requested = false;
    }

    fn block_current_until(&mut self, active_frame_words: *mut u64, deadline: u64) {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("sleep requested without a running task"));
        let next = self.select_next_excluding(current);

        // Save the current task's post-SVC resume frame before making it unrunnable.
        copy_words(
            self.tasks[current].frame.as_mut_words_ptr(),
            active_frame_words as *const u64,
        );

        copy_words(active_frame_words, self.tasks[next].frame.as_words_ptr());
        self.commit_block(current, next);
        let now = crate::time::now_counter();
        crate::time::schedule_event(deadline, TimedEvent::WakeTask(current));
        self.replace_quantum_event(now, current);
        crate::trace!("sched: task {current} sleeping until counter {deadline}");
        if next != current {
            log_switch(current, next);
        }
    }

    pub(crate) fn enter_running_task(&mut self) -> ! {
        let running = self
            .running_task()
            .unwrap_or_else(|| panic!("scheduler has no running task"));
        let frame_words = self.tasks[running].frame.as_words_ptr();
        let now = crate::time::now_counter();

        self.entered_running_task = true;
        self.ensure_quantum_event(now);
        debug_assert_eq!(self.current, Some(running));
        debug_assert_eq!(self.tasks[running].state, TaskState::Running);
        // SAFETY: scheduler bootstrap prepared the running task trap frame.
        unsafe { arch_enter_task_frame(frame_words) }
    }
}

impl Scheduler {
    fn init(&mut self, rr_quantum_ms: u64) {
        for id in 0..MAX_TASKS {
            let task = &mut self.tasks[id];
            task.id = id;
            task.priority = 0;
            task.state = TaskState::Blocked;
            task.entry = parked_task;
            task.stack_top = self.stacks[id].top();
            task.frame.clear();
        }

        self.tasks[IDLE_TASK_ID].priority = IDLE_PRIORITY;
        self.tasks[IDLE_TASK_ID].state = TaskState::Ready;
        self.current = None;
        self.rr_quantum_ms = rr_quantum_ms.max(1);
        self.resched_requested = false;
        self.entered_running_task = false;
    }

    fn set_idle_task(&mut self, entry: TaskEntry) {
        self.tasks[IDLE_TASK_ID].entry = entry;
        self.tasks[IDLE_TASK_ID].priority = IDLE_PRIORITY;
        self.make_ready(IDLE_TASK_ID);
    }

    fn add_ready_task(&mut self, priority: u8, entry: TaskEntry) -> Result<TaskId> {
        let id = self
            .find_free_task_slot()
            .ok_or(SchedError::NoFreeTaskSlot)?;

        self.tasks[id].id = id;
        self.tasks[id].priority = priority;
        self.tasks[id].entry = entry;
        self.make_ready(id);
        Ok(id)
    }

    fn initialize_task_frame(&mut self, id: TaskId) -> Result<()> {
        if id >= MAX_TASKS {
            return Err(SchedError::InvalidTaskId);
        }

        if self.is_free_slot(id) {
            return Err(SchedError::AlreadyScheduled);
        }

        let task = &mut self.tasks[id];

        // SAFETY: scheduler owns frame storage, stack top and entry pointer are validated by task table setup.
        unsafe {
            arch_init_task_frame(
                task.frame.as_mut_words_ptr(),
                task.stack_top,
                task.entry as usize,
                task_entry_bootstrap as usize,
            );
        }
        Ok(())
    }

    fn running_task(&self) -> Option<TaskId> {
        self.current
    }

    fn mark_initial_running(&mut self, id: TaskId) -> Result<()> {
        if id >= MAX_TASKS {
            return Err(SchedError::InvalidTaskId);
        }

        if self.tasks[id].state == TaskState::Blocked {
            return Err(SchedError::AlreadyScheduled);
        }

        self.make_ready(IDLE_TASK_ID);
        self.make_running(id);
        debug_assert_eq!(self.tasks[id].state, TaskState::Running);
        debug_assert_eq!(self.current, Some(id));
        Ok(())
    }

    fn select_next(&self) -> TaskId {
        self.pick_next_excluding(self.current.unwrap_or(IDLE_TASK_ID), None)
            .unwrap_or(IDLE_TASK_ID)
    }

    // Round-robin continuation used when the current running task is about to
    // stop being runnable (for example, it goes to sleep). We continue the scan
    // after the current slot, but explicitly skip selecting that task again.
    fn select_next_excluding(&self, excluded: TaskId) -> TaskId {
        self.pick_next_excluding(self.current.unwrap_or(IDLE_TASK_ID), Some(excluded))
            .unwrap_or(IDLE_TASK_ID)
    }

    fn commit_switch(&mut self, prev: TaskId, next: TaskId) {
        if prev == next {
            return;
        }

        debug_assert_eq!(self.current, Some(prev));
        debug_assert_eq!(self.tasks[prev].state, TaskState::Running);
        debug_assert_ne!(self.tasks[next].state, TaskState::Blocked);

        self.make_ready(prev);
        self.make_running(next);
        debug_assert_eq!(self.current, Some(next));
        debug_assert_eq!(self.tasks[next].state, TaskState::Running);
        debug_assert_eq!(self.tasks[prev].state, TaskState::Ready);
    }

    fn commit_block(&mut self, blocked: TaskId, next: TaskId) {
        debug_assert_eq!(self.current, Some(blocked));
        debug_assert_eq!(self.tasks[blocked].state, TaskState::Running);
        debug_assert_ne!(self.tasks[next].state, TaskState::Blocked);

        self.make_blocked(blocked);
        self.make_running(next);

        debug_assert_eq!(self.current, Some(next));
        debug_assert_eq!(self.tasks[blocked].state, TaskState::Blocked);
        debug_assert_eq!(self.tasks[next].state, TaskState::Running);
    }
}

impl Scheduler {
    fn wake_task(&mut self, task_id: TaskId) {
        if task_id >= MAX_TASKS {
            return;
        }

        if self.is_free_slot(task_id) {
            return;
        }

        if self.tasks[task_id].state == TaskState::Blocked {
            self.make_ready(task_id);
            crate::trace!("sched: task {task_id} moved to Ready");
        }
    }

    fn note_quantum_expired(&mut self, task_id: TaskId) {
        if self.current == Some(task_id) {
            self.resched_requested = true;
            crate::trace!("sched: quantum expired task {task_id}");
        }
    }

    fn pick_next_excluding(&self, start: TaskId, excluded: Option<TaskId>) -> Option<TaskId> {
        for offset in 1..=MAX_TASKS {
            let id = (start + offset) % MAX_TASKS;
            if id == IDLE_TASK_ID {
                continue;
            }

            if Some(id) == excluded {
                continue;
            }

            if self.tasks[id].state != TaskState::Blocked {
                return Some(id);
            }
        }

        None
    }

    fn find_free_task_slot(&self) -> Option<TaskId> {
        ((IDLE_TASK_ID + 1)..MAX_TASKS).find(|&id| self.is_free_slot(id))
    }

    fn make_ready(&mut self, id: TaskId) {
        self.tasks[id].state = TaskState::Ready;
    }

    fn make_running(&mut self, id: TaskId) {
        self.tasks[id].state = TaskState::Running;
        self.current = Some(id);
    }

    fn make_blocked(&mut self, id: TaskId) {
        self.tasks[id].state = TaskState::Blocked;
    }

    fn is_free_slot(&self, id: TaskId) -> bool {
        let task = &self.tasks[id];
        task.state == TaskState::Blocked && task.entry as usize == parked_task as usize
    }

    fn has_runnable_peer(&self, current: TaskId) -> bool {
        for id in 0..MAX_TASKS {
            if id == current || id == IDLE_TASK_ID {
                continue;
            }

            if self.tasks[id].state != TaskState::Blocked {
                return true;
            }
        }

        false
    }

    fn ensure_quantum_event(&mut self, now: u64) {
        let Some(current) = self.current else {
            return;
        };

        let event = TimedEvent::QuantumExpired(current);
        if !self.has_runnable_peer(current) {
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
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Task {
    const fn blocked(id: TaskId) -> Self {
        Self {
            id,
            priority: 0,
            state: TaskState::Blocked,
            entry: parked_task,
            stack_top: 0,
            frame: TaskFrameStorage::zeroed(),
        }
    }
}

#[inline(always)]
fn copy_words(dst: *mut u64, src: *const u64) {
    // SAFETY: caller guarantees both buffers are valid and non-overlapping `TASK_FRAME_WORDS` storage.
    unsafe {
        for i in 0..TASK_FRAME_WORDS {
            *dst.add(i) = *src.add(i);
        }
    }
}

fn parked_task() -> ! {
    loop {
        core::hint::spin_loop();
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

fn log_switch(prev: usize, next: usize) {
    crate::trace!("sched: prev={prev} next={next}");
}
