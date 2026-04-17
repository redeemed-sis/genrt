use core::{cell::UnsafeCell, mem};

pub(crate) type TaskId = usize;
pub(crate) type TaskEntry = fn() -> !;
pub(crate) type Result<T> = core::result::Result<T, SchedError>;

pub(crate) const MAX_TASKS: usize = 8;
const TASK_STACK_SIZE: usize = 4096;
const IDLE_TASK_ID: TaskId = 0;
const IDLE_PRIORITY: u8 = 0;

use crate::arch_consts::TASK_FRAME_WORDS;

unsafe extern "C" {
    fn arch_init_task_frame(
        frame_words: *mut u64,
        stack_top: usize,
        entry_addr: usize,
        bootstrap_pc: usize,
    );
    fn arch_enter_task_frame(frame_words: *const u64) -> !;
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
    entered_running_task: bool,
}

pub(crate) fn bootstrap(idle_entry: TaskEntry, tasks: &[StaticTask]) -> Result<()> {
    let sched = scheduler_mut();
    sched.bootstrap(idle_entry, tasks)
}

#[unsafe(no_mangle)]
pub extern "C" fn on_tick_interrupt(active_frame_words: *mut u64) {
    if active_frame_words.is_null() {
        return;
    }

    crate::time::on_tick_interrupt();
    scheduler_mut().preempt_on_tick(active_frame_words);
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
            entered_running_task: false,
        }
    }
}

impl Scheduler {
    fn bootstrap(&mut self, idle_entry: TaskEntry, tasks: &[StaticTask]) -> Result<()> {
        self.init();
        self.set_idle_task(idle_entry);
        self.initialize_task_frame(IDLE_TASK_ID)?;
        for task in tasks {
            let id = self.add_ready_task(task.priority, task.entry)?;
            self.initialize_task_frame(id)?;
        }
        self.mark_initial_running(IDLE_TASK_ID)?;
        Ok(())
    }

    pub(crate) fn preempt_on_tick(&mut self, active_frame_words: *mut u64) {
        if !self.entered_running_task {
            return;
        }

        let current = match self.running_task() {
            Some(id) => id,
            None => return,
        };

        let next = self.select_next();
        if next == current {
            return;
        }

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

    pub(crate) fn enter_running_task(&mut self) -> ! {
        let running = self
            .running_task()
            .unwrap_or_else(|| panic!("scheduler has no running task"));
        let frame_words = self.tasks[running].frame.as_words_ptr();

        self.entered_running_task = true;
        debug_assert_eq!(self.current, Some(running));
        debug_assert_eq!(self.tasks[running].state, TaskState::Running);
        // SAFETY: scheduler bootstrap prepared the running task trap frame.
        unsafe { arch_enter_task_frame(frame_words) }
    }
}

impl Scheduler {
    fn init(&mut self) {
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
        self.entered_running_task = false;
    }

    fn set_idle_task(&mut self, entry: TaskEntry) {
        self.tasks[IDLE_TASK_ID].entry = entry;
        self.tasks[IDLE_TASK_ID].priority = IDLE_PRIORITY;
        self.tasks[IDLE_TASK_ID].state = TaskState::Ready;
    }

    fn add_ready_task(&mut self, priority: u8, entry: TaskEntry) -> Result<TaskId> {
        let id = self
            .find_free_task_slot()
            .ok_or(SchedError::NoFreeTaskSlot)?;

        self.tasks[id].id = id;
        self.tasks[id].priority = priority;
        self.tasks[id].entry = entry;
        self.tasks[id].state = TaskState::Ready;
        Ok(id)
    }

    fn initialize_task_frame(&mut self, id: TaskId) -> Result<()> {
        if id >= MAX_TASKS {
            return Err(SchedError::InvalidTaskId);
        }

        let task = &mut self.tasks[id];
        if task.state == TaskState::Blocked {
            return Err(SchedError::AlreadyScheduled);
        }

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

        self.tasks[IDLE_TASK_ID].state = TaskState::Ready;
        self.tasks[id].state = TaskState::Running;
        self.current = Some(id);
        debug_assert_eq!(self.tasks[id].state, TaskState::Running);
        debug_assert_eq!(self.current, Some(id));
        Ok(())
    }

    fn select_next(&self) -> TaskId {
        self.pick_next().unwrap_or(IDLE_TASK_ID)
    }

    fn commit_switch(&mut self, prev: TaskId, next: TaskId) {
        if prev == next {
            return;
        }

        debug_assert_eq!(self.current, Some(prev));
        debug_assert_eq!(self.tasks[prev].state, TaskState::Running);
        debug_assert_ne!(self.tasks[next].state, TaskState::Blocked);

        if self.tasks[prev].state == TaskState::Running {
            self.tasks[prev].state = TaskState::Ready;
        }

        self.tasks[next].state = TaskState::Running;
        self.current = Some(next);
        debug_assert_eq!(self.current, Some(next));
        debug_assert_eq!(self.tasks[next].state, TaskState::Running);
        debug_assert_eq!(self.tasks[prev].state, TaskState::Ready);
    }
}

impl Scheduler {
    fn pick_next(&self) -> Option<TaskId> {
        let start = self.current.unwrap_or(IDLE_TASK_ID);

        for offset in 1..=MAX_TASKS {
            let id = (start + offset) % MAX_TASKS;
            if id == IDLE_TASK_ID {
                continue;
            }

            if self.tasks[id].state != TaskState::Blocked {
                return Some(id);
            }
        }

        None
    }

    fn find_free_task_slot(&self) -> Option<TaskId> {
        ((IDLE_TASK_ID + 1)..MAX_TASKS).find(|&id| self.tasks[id].state == TaskState::Blocked)
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
