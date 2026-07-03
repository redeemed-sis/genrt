use alloc::boxed::Box;

use crate::{
    arch_consts::TASK_FRAME_WORDS,
    memory::vm::UserAddressSpace,
    process::ProcessId,
    task::{TaskId, ThreadId},
    time::TimedEvent,
};

use super::{
    IDLE_TASK_ID, INITIAL_THREAD_GENERATION, Scheduler, THREAD_STACK_SIZE, arch_enter_task_frame,
    copy_words, ipc as sched_ipc, log_switch, scheduler_mut, thread,
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum BlockReason {
    Sleep,
    Ipc(sched_ipc::IpcBlock),
    Join(ThreadId),
    ProcessJoin(ProcessId),
}

// Task-state semantics:
// - Free: preallocated slot, stack, and frame storage are available for spawn.
// - Running: the sole task whose saved frame matches the scheduler's committed resume target.
// - Ready: runnable, owns a valid saved frame, and sits in the ready queue unless it is idle.
// - Blocked(reason): not runnable and not considered during round-robin
//   selection. Wakeup ownership still lives outside the scheduler: deadlines in
//   `kernel::time` and wait queues in `kernel::ipc`. The scheduler keeps an
//   opaque IPC wait token so timeout dispatch can ask IPC to remove the waiter
//   before restoring the task to `Ready`, without knowing the concrete IPC type.
// - Zombie: joinable thread has exited and keeps its slot until a successful
//   join observes the exit code and reclaims it.
//
// The idle thread is never joinable, never reclaimed, and must not enter
// `thread_exit`; it remains the fallback runnable thread when no ordinary thread
// is ready.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum TaskState {
    Free,
    Ready,
    Running,
    Blocked(BlockReason),
    Zombie,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum ThreadKind {
    Kernel,
    User {
        process_id: ProcessId,
        address_space: UserAddressSpace,
    },
}

#[repr(C, align(16))]
struct TaskStack {
    bytes: [u8; THREAD_STACK_SIZE],
}

impl TaskStack {
    const fn zeroed() -> Self {
        Self {
            bytes: [0; THREAD_STACK_SIZE],
        }
    }

    fn top(&self) -> usize {
        self.bytes.as_ptr() as usize + THREAD_STACK_SIZE
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

pub(super) struct Task {
    pub(super) generation: u32,
    pub(super) state: TaskState,
    pub(super) joinable: bool,
    pub(super) exit_code: Option<usize>,
    pub(super) joiner: Option<ThreadId>,
    pub(super) ipc: sched_ipc::TaskIpcState,
    pub(super) last_join_result: Option<core::result::Result<usize, thread::JoinError>>,
    pub(super) entry: thread::ThreadEntry,
    pub(super) arg: thread::ThreadArg,
    pub(super) kind: ThreadKind,
    stack: Box<TaskStack>,
    frame: Box<TaskFrameStorage>,
}

impl Task {
    pub(super) fn bootstrap(
        entry: thread::ThreadEntry,
        arg: thread::ThreadArg,
        state: TaskState,
        joinable: bool,
    ) -> Self {
        Self {
            generation: INITIAL_THREAD_GENERATION,
            state,
            joinable,
            exit_code: None,
            joiner: None,
            ipc: sched_ipc::TaskIpcState::empty(),
            last_join_result: None,
            entry,
            arg,
            kind: ThreadKind::Kernel,
            stack: Box::new(TaskStack::zeroed()),
            frame: Box::new(TaskFrameStorage::zeroed()),
        }
    }

    pub(super) fn free() -> Self {
        Self {
            generation: 0,
            state: TaskState::Free,
            joinable: false,
            exit_code: None,
            joiner: None,
            ipc: sched_ipc::TaskIpcState::empty(),
            last_join_result: None,
            entry: thread::free_task_entry,
            arg: thread::ThreadArg::empty(),
            kind: ThreadKind::Kernel,
            stack: Box::new(TaskStack::zeroed()),
            frame: Box::new(TaskFrameStorage::zeroed()),
        }
    }

    pub(super) fn stack_top(&self) -> usize {
        self.stack.top()
    }

    pub(super) fn is_blocked(&self) -> bool {
        matches!(self.state, TaskState::Blocked(_))
    }

    pub(super) fn is_free(&self) -> bool {
        self.state == TaskState::Free
    }

    pub(super) fn is_runnable(&self) -> bool {
        matches!(self.state, TaskState::Ready | TaskState::Running)
    }
}

pub(super) fn finish_timer_interrupt(active_frame_words: *mut u64, now: u64) {
    if active_frame_words.is_null() {
        return;
    }

    scheduler_mut().finish_timer_interrupt(active_frame_words, now);
}

pub(super) fn on_wake_task(task_id: TaskId) {
    wake_task(task_id);
}

pub(super) fn on_quantum_expired(task_id: TaskId) {
    scheduler_mut().note_quantum_expired(task_id);
}

pub fn current_task_id() -> Option<TaskId> {
    scheduler_mut().running_task()
}

pub fn wake_task(task_id: TaskId) {
    scheduler_mut().wake_task(task_id);
}

pub(crate) fn enter_running_task() -> ! {
    scheduler_mut().enter_running_task()
}

impl Scheduler {
    pub(super) fn finish_timer_interrupt(&mut self, active_frame_words: *mut u64, now: u64) {
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

    pub(super) fn begin_block_current(
        &mut self,
        active_frame_words: *mut u64,
        reason: BlockReason,
    ) -> (TaskId, TaskId) {
        let current = self.blocking_current(active_frame_words);
        let next = self.block_current_with_reason(active_frame_words, current, reason);
        (current, next)
    }

    pub(super) fn blocking_current(&self, active_frame_words: *mut u64) -> TaskId {
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

    pub(super) fn block_current_with_reason(
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

    pub(super) fn finish_block_current(&mut self, current: TaskId, next: TaskId) {
        let now = crate::time::now_counter();
        self.replace_quantum_event(now, current);
        if next != current {
            log_switch(current, next);
        }
    }

    pub(super) fn enter_running_task(&mut self) -> ! {
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

    pub(super) fn running_task(&self) -> Option<TaskId> {
        self.current
    }

    pub(super) fn mark_initial_running(&mut self, id: TaskId) -> super::Result<()> {
        if !self.is_valid_task(id) {
            return Err(super::SchedError::InvalidTaskId);
        }

        if !self.task(id).is_runnable() {
            return Err(super::SchedError::InvalidTaskId);
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
        debug_assert_eq!(self.task(next).state, TaskState::Ready);

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
        debug_assert_eq!(self.task(next).state, TaskState::Ready);

        self.make_blocked(blocked, reason);
        self.make_running(next);

        debug_assert_eq!(self.current, Some(next));
        debug_assert!(self.task(blocked).is_blocked());
        debug_assert_eq!(self.task(next).state, TaskState::Running);
    }

    pub(super) fn wake_task(&mut self, task_id: TaskId) {
        if !self.is_valid_task(task_id) {
            return;
        }

        if self.task(task_id).is_blocked() {
            self.make_ready_and_queue(task_id);
            crate::trace!("sched: task {task_id} moved to Ready");
        }
    }

    pub(super) fn note_quantum_expired(&mut self, task_id: TaskId) {
        if self.current == Some(task_id) {
            self.resched_requested = true;
            crate::trace!("sched: quantum expired task {task_id}");
        }
    }

    pub(super) fn ready_push_back(&mut self, id: TaskId) {
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

    pub(super) fn dequeue_next_ready(&mut self) -> Option<TaskId> {
        self.ready_queue.pop_front()
    }

    pub(super) fn make_ready(&mut self, id: TaskId) {
        self.task_mut(id).state = TaskState::Ready;
    }

    pub(super) fn make_ready_and_queue(&mut self, id: TaskId) {
        self.make_ready(id);
        if id != IDLE_TASK_ID {
            let was_empty = self.ready_queue.is_empty();
            self.ready_push_back(id);
            if was_empty {
                self.note_runnable_peer_available();
            }
        }
    }

    pub(super) fn make_running(&mut self, id: TaskId) {
        self.activate_task_address_space(id);
        self.task_mut(id).state = TaskState::Running;
        self.current = Some(id);
    }

    fn make_blocked(&mut self, id: TaskId, reason: BlockReason) {
        self.task_mut(id).state = TaskState::Blocked(reason);
    }

    fn activate_task_address_space(&self, id: TaskId) {
        let result = match self.task(id).kind {
            ThreadKind::Kernel => unsafe { crate::memory::vm::clear_user_address_space() },
            ThreadKind::User { address_space, .. } => unsafe {
                crate::memory::vm::activate_user_address_space(address_space)
            },
        };

        if let Err(err) = result {
            panic!("sched: failed to activate address space for task {id}: {err:?}");
        }
    }

    fn has_runnable_peer(&self) -> bool {
        !self.ready_queue.is_empty()
    }

    pub(super) fn note_runnable_peer_available(&mut self) {
        if self.current.is_some() && self.has_runnable_peer() {
            self.ensure_quantum_event(crate::time::now_counter());
        }
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

    pub(super) fn replace_quantum_event(&mut self, now: u64, obsolete_task: TaskId) {
        crate::time::cancel_event(TimedEvent::QuantumExpired(obsolete_task));
        self.ensure_quantum_event(now);
    }

    pub(super) fn is_valid_task(&self, id: TaskId) -> bool {
        id.index() < self.tasks.len()
    }

    pub(super) fn task(&self, id: TaskId) -> &Task {
        &self.tasks[id.index()]
    }

    pub(super) fn task_mut(&mut self, id: TaskId) -> &mut Task {
        &mut self.tasks[id.index()]
    }

    pub(super) fn frame_words_ptr(&self, id: TaskId) -> *const u64 {
        self.task(id).frame.as_words_ptr()
    }

    pub(super) fn frame_words_mut_ptr(&mut self, id: TaskId) -> *mut u64 {
        self.task_mut(id).frame.as_mut_words_ptr()
    }
}
