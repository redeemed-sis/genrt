//! Scheduler lifecycle transitions and bounded state validation.
//!
//! This module is the sole owner of task lifecycle state, current-task identity,
//! and ready-queue membership. Handoff code supplies architecture context save /
//! restore and address-space activation after a transition selects its outcome.

use alloc::{boxed::Box, collections::VecDeque, vec::Vec};

use crate::{
    arch::SavedContext,
    memory::vm::UserAddressSpace,
    process::ProcessId,
    task::{TaskId, ThreadId},
};

use super::{
    IDLE_TASK_ID, INITIAL_THREAD_GENERATION, Scheduler, THREAD_STACK_SIZE, ipc as sched_ipc, thread,
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum BlockReason {
    Sleep,
    Ipc(sched_ipc::IpcBlock),
    Join(ThreadId),
    Process(ProcessId),
    StdinRead,
}

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

/// A context-free scheduling decision for architecture handoff code.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum SwitchOutcome {
    ContinueCurrent,
    Switch { from: ThreadId, to: ThreadId },
}

#[repr(C, align(16))]
struct TaskStack {
    bytes: [u8; THREAD_STACK_SIZE],
}

impl TaskStack {
    fn top(&self) -> usize {
        self.bytes.as_ptr() as usize + THREAD_STACK_SIZE
    }
}

struct Task {
    generation: u32,
    state: TaskState,
    joinable: bool,
    exit_code: Option<usize>,
    joiner: Option<ThreadId>,
    ipc: sched_ipc::TaskIpcState,
    last_join_result: Option<core::result::Result<usize, thread::JoinError>>,
    kind: ThreadKind,
    stack: Box<TaskStack>,
    context: Option<SavedContext>,
}

impl Task {
    fn free() -> Self {
        Self {
            generation: 0,
            state: TaskState::Free,
            joinable: false,
            exit_code: None,
            joiner: None,
            ipc: sched_ipc::TaskIpcState::empty(),
            last_join_result: None,
            kind: ThreadKind::Kernel,
            stack: boxed_zeroed_stack(),
            context: None,
        }
    }

    fn stack_top(&self) -> usize {
        self.stack.top()
    }
}

pub(super) struct TransitionState {
    tasks: Vec<Task>,
    ready_queue: VecDeque<ThreadId>,
    current: Option<ThreadId>,
}

impl TransitionState {
    fn with_capacity(task_capacity: usize) -> Self {
        let mut tasks = Vec::new();
        tasks.reserve_exact(task_capacity);

        let mut ready_queue = VecDeque::new();
        ready_queue.reserve_exact(task_capacity.saturating_sub(1));

        Self {
            tasks,
            ready_queue,
            current: None,
        }
    }
}

fn boxed_zeroed_stack() -> Box<TaskStack> {
    let mut boxed = Box::<TaskStack>::new_uninit();
    // SAFETY: TaskStack is a byte array whose all-zero value is valid.
    unsafe {
        core::ptr::write_bytes(
            boxed.as_mut_ptr().cast::<u8>(),
            0,
            core::mem::size_of::<TaskStack>(),
        );
        boxed.assume_init()
    }
}

impl Scheduler {
    pub(super) fn transition_new(task_capacity: usize, rr_quantum_ms: u64) -> Self {
        Self {
            lifecycle: TransitionState::with_capacity(task_capacity),
            rr_quantum_ms: rr_quantum_ms.max(1),
            entered_running_task: false,
        }
    }

    pub(super) fn transition_append_bootstrap(
        &mut self,
        entry: thread::ThreadEntry,
        arg: thread::ThreadArg,
        idle: bool,
    ) -> TaskId {
        let id = TaskId::new(self.lifecycle.tasks.len());
        self.lifecycle.tasks.push(Task::free());
        self.transition_publish_bootstrap(id, entry, arg, idle);
        id
    }

    pub(super) fn transition_fill_free_slots(&mut self, task_capacity: usize) {
        while self.lifecycle.tasks.len() < task_capacity {
            let id = TaskId::new(self.lifecycle.tasks.len());
            self.lifecycle.tasks.push(Task::free());
            crate::trace!("sched: prepared free thread slot {id}");
        }
    }

    pub(super) fn transition_task_count(&self) -> usize {
        self.lifecycle.tasks.len()
    }

    pub(super) fn transition_ready_capacity(&self) -> usize {
        self.lifecycle.ready_queue.capacity()
    }

    pub(super) fn transition_has_ready(&self) -> bool {
        !self.lifecycle.ready_queue.is_empty()
    }

    pub(super) fn find_free_slot(&self) -> Option<TaskId> {
        self.lifecycle
            .tasks
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(index, task)| (task.state == TaskState::Free).then_some(TaskId::new(index)))
    }

    pub(super) fn transition_publish_bootstrap(
        &mut self,
        id: TaskId,
        entry: thread::ThreadEntry,
        arg: thread::ThreadArg,
        idle: bool,
    ) {
        if idle != (id == IDLE_TASK_ID) {
            panic!("sched: bootstrap idle identity mismatch");
        }
        let context = SavedContext::kernel_entry(
            self.lifecycle.tasks[id.index()].stack_top(),
            entry as *const () as usize,
            arg.as_usize(),
            thread::thread_entry_bootstrap as *const () as usize,
        );
        self.publish(
            id,
            context,
            ThreadKind::Kernel,
            false,
            INITIAL_THREAD_GENERATION,
            true,
        );
    }

    pub(super) fn transition_publish_runtime(
        &mut self,
        id: TaskId,
        context: SavedContext,
        kind: ThreadKind,
        joinable: bool,
    ) -> ThreadId {
        let generation = next_generation(self.lifecycle.tasks[id.index()].generation);
        self.publish(id, context, kind, joinable, generation, false);
        self.thread_id(id)
    }

    fn publish(
        &mut self,
        id: TaskId,
        context: SavedContext,
        kind: ThreadKind,
        joinable: bool,
        generation: u32,
        bootstrap: bool,
    ) {
        let task = &mut self.lifecycle.tasks[id.index()];
        if task.state != TaskState::Free || task.context.is_some() {
            panic!("sched: publish into occupied slot {id}");
        }
        task.generation = generation;
        task.joinable = joinable;
        task.exit_code = None;
        task.joiner = None;
        task.ipc.reset();
        task.last_join_result = None;
        task.kind = kind;
        task.context = Some(context);
        task.state = TaskState::Ready;
        if id != IDLE_TASK_ID {
            let was_empty = self.lifecycle.ready_queue.is_empty();
            self.ready_push_back(self.thread_id(id));
            if !bootstrap && was_empty {
                self.note_ready_peer();
            }
        }
        self.validate_after_transition();
    }

    pub(super) fn transition_initial_dispatch(&mut self) -> super::Result<()> {
        if self.lifecycle.current.is_some() {
            return Err(super::SchedError::InvalidTaskId);
        }
        let next = self
            .ready_pop_front()
            .unwrap_or_else(|| self.thread_id(IDLE_TASK_ID));
        self.make_running(next);
        self.validate_after_transition();
        Ok(())
    }

    pub(super) fn transition_optional_switch(&mut self) -> SwitchOutcome {
        let Some(from) = self.lifecycle.current else {
            return SwitchOutcome::ContinueCurrent;
        };
        self.assert_current_running(from, "optional switch");
        let Some(to) = self.ready_pop_front() else {
            self.validate_after_transition();
            return SwitchOutcome::ContinueCurrent;
        };
        // The checkpoint already consumed the request that selected this
        // switch. Requeueing the outgoing task must not create a second one.
        self.make_ready_and_queue(from, false);
        self.make_running(to);
        self.validate_after_transition();
        SwitchOutcome::Switch { from, to }
    }

    pub(super) fn transition_block_current(&mut self, reason: BlockReason) -> SwitchOutcome {
        crate::sync::preempt::assert_preemption_enabled("scheduler blocking transition");
        let from = self
            .lifecycle
            .current
            .unwrap_or_else(|| panic!("block requested without a running task"));
        self.assert_current_running(from, "blocking transition");
        if from.index() == IDLE_TASK_ID.index() {
            panic!("sched: idle task cannot block");
        }
        let to = self
            .ready_pop_front()
            .unwrap_or_else(|| self.thread_id(IDLE_TASK_ID));
        self.task_mut(TaskId::new(from.index())).state = TaskState::Blocked(reason);
        self.make_running(to);
        self.validate_after_transition();
        SwitchOutcome::Switch { from, to }
    }

    pub(super) fn transition_wake_thread(&mut self, id: ThreadId, expected: BlockReason) -> bool {
        if !self.thread_matches(id)
            || self.task_state(TaskId::new(id.index())) != TaskState::Blocked(expected)
        {
            return false;
        }
        self.make_ready_and_queue(id, true);
        self.validate_after_transition();
        true
    }

    pub(super) fn transition_exit_current(&mut self, code: usize) -> SwitchOutcome {
        let from = self
            .lifecycle
            .current
            .unwrap_or_else(|| panic!("thread: exit without running thread"));
        self.assert_current_running(from, "exit transition");
        if from.index() == IDLE_TASK_ID.index() {
            panic!("thread: idle thread cannot exit");
        }
        let joinable = self.task(TaskId::new(from.index())).joinable;
        let joiner = self.task(TaskId::new(from.index())).joiner;
        self.task_mut(TaskId::new(from.index())).exit_code = Some(code);
        self.task_mut(TaskId::new(from.index())).state = TaskState::Zombie;
        if let Some(joiner) = joiner {
            self.complete_joiner(from, joiner, code);
            self.reap_zombie(from);
        } else if !joinable {
            self.reap_zombie(from);
        }
        let to = self
            .ready_pop_front()
            .unwrap_or_else(|| self.thread_id(IDLE_TASK_ID));
        self.make_running(to);
        self.validate_after_transition();
        SwitchOutcome::Switch { from, to }
    }

    pub(super) fn transition_reap_zombie(&mut self, id: ThreadId) {
        if id.index() >= self.lifecycle.tasks.len()
            || !self.thread_matches(id)
            || id.index() == IDLE_TASK_ID.index()
            || self.lifecycle.current == Some(id)
            || self.task(TaskId::new(id.index())).state != TaskState::Zombie
        {
            panic!("thread: reclaim requires a non-current zombie");
        }
        if self
            .lifecycle
            .ready_queue
            .iter()
            .any(|queued| queued.index() == id.index())
        {
            panic!("thread: reclaim target still queued ready");
        }
        self.reap_zombie(id);
        self.validate_after_transition();
    }

    fn reap_zombie(&mut self, id: ThreadId) {
        let task = self.task_mut(TaskId::new(id.index()));
        task.joinable = false;
        task.exit_code = None;
        task.joiner = None;
        task.ipc.reset();
        task.last_join_result = None;
        task.kind = ThreadKind::Kernel;
        task.context = None;
        task.state = TaskState::Free;
    }

    fn complete_joiner(&mut self, target: ThreadId, joiner: ThreadId, code: usize) {
        if !self.matches_blocked(joiner, BlockReason::Join(target)) {
            panic!("thread: joiner {joiner} has invalid state while target {target} exited");
        }
        self.task_mut(TaskId::new(joiner.index())).last_join_result = Some(Ok(code));
        self.make_ready_and_queue(joiner, true);
    }

    fn ready_push_back(&mut self, id: ThreadId) {
        if id.index() == IDLE_TASK_ID.index() {
            panic!("sched: idle must not enter ready queue");
        }
        if let Some(queued) = self
            .lifecycle
            .ready_queue
            .iter()
            .find(|queued| queued.index() == id.index())
        {
            panic!("sched: duplicate or stale ready queue entry {queued} for {id}");
        }
        if self.lifecycle.ready_queue.len() == self.lifecycle.ready_queue.capacity() {
            panic!("sched: ready queue capacity exhausted");
        }
        self.lifecycle.ready_queue.push_back(id);
    }

    fn ready_pop_front(&mut self) -> Option<ThreadId> {
        let id = self.lifecycle.ready_queue.pop_front()?;
        if !self.thread_matches(id)
            || self.task(TaskId::new(id.index())).state != TaskState::Ready
            || id.index() == IDLE_TASK_ID.index()
        {
            panic!("sched: stale or invalid ready queue entry {id}");
        }
        Some(id)
    }

    fn make_ready_and_queue(&mut self, id: ThreadId, notify_new_peer: bool) {
        if !self.thread_matches(id) {
            panic!("sched: ready transition requires a current-generation task {id}");
        }
        self.task_mut(TaskId::new(id.index())).state = TaskState::Ready;
        if id.index() != IDLE_TASK_ID.index() {
            let was_empty = self.lifecycle.ready_queue.is_empty();
            self.ready_push_back(id);
            if notify_new_peer && was_empty {
                self.note_ready_peer();
            }
        }
    }

    fn assert_current_running(&self, id: ThreadId, operation: &str) {
        if self.lifecycle.current != Some(id)
            || !self.thread_matches(id)
            || self.task_state(TaskId::new(id.index())) != TaskState::Running
        {
            panic!("sched: {operation} requires the current running task {id}");
        }
    }

    fn make_running(&mut self, id: ThreadId) {
        if !self.thread_matches(id) || self.task(TaskId::new(id.index())).state != TaskState::Ready
        {
            panic!("sched: running transition requires a ready current-generation task");
        }
        self.task_mut(TaskId::new(id.index())).state = TaskState::Running;
        self.lifecycle.current = Some(id);
    }

    fn note_ready_peer(&mut self) {
        self.note_runnable_peer_available();
    }

    pub(super) fn current_thread(&self) -> Option<ThreadId> {
        self.lifecycle.current
    }

    pub(super) fn thread_id(&self, id: TaskId) -> ThreadId {
        ThreadId::new(id.index(), self.task(id).generation)
    }

    pub(super) fn thread_matches(&self, id: ThreadId) -> bool {
        self.is_valid_task(TaskId::new(id.index()))
            && self.task(TaskId::new(id.index())).generation == id.generation()
            && self.task(TaskId::new(id.index())).state != TaskState::Free
    }

    pub(super) fn is_valid_task(&self, id: TaskId) -> bool {
        id.index() < self.lifecycle.tasks.len()
    }

    fn task(&self, id: TaskId) -> &Task {
        &self.lifecycle.tasks[id.index()]
    }

    fn task_mut(&mut self, id: TaskId) -> &mut Task {
        &mut self.lifecycle.tasks[id.index()]
    }

    pub(super) fn task_state(&self, id: TaskId) -> TaskState {
        self.task(id).state
    }

    pub(super) fn task_kind(&self, id: TaskId) -> ThreadKind {
        self.task(id).kind
    }

    pub(super) fn saved_context(&self, id: ThreadId) -> &SavedContext {
        self.task(TaskId::new(id.index()))
            .context
            .as_ref()
            .unwrap_or_else(|| panic!("sched: occupied task {} has no saved context", id.index()))
    }

    pub(super) fn saved_context_mut(&mut self, id: ThreadId) -> &mut SavedContext {
        self.task_mut(TaskId::new(id.index()))
            .context
            .as_mut()
            .unwrap_or_else(|| panic!("sched: occupied task {} has no saved context", id.index()))
    }

    pub(super) fn stack_top(&self, id: TaskId) -> usize {
        self.task(id).stack_top()
    }

    pub(super) fn task_joinable(&self, id: TaskId) -> bool {
        self.task(id).joinable
    }
    pub(super) fn task_joiner(&self, id: TaskId) -> Option<ThreadId> {
        self.task(id).joiner
    }
    pub(super) fn task_exit_code(&self, id: TaskId) -> Option<usize> {
        self.task(id).exit_code
    }
    pub(super) fn task_set_joiner(&mut self, id: TaskId, joiner: ThreadId) {
        self.task_mut(id).joiner = Some(joiner);
    }
    pub(super) fn set_join_result(
        &mut self,
        id: TaskId,
        result: core::result::Result<usize, thread::JoinError>,
    ) {
        self.task_mut(id).last_join_result = Some(result);
    }
    pub(super) fn take_join_result(
        &mut self,
        id: TaskId,
    ) -> Option<core::result::Result<usize, thread::JoinError>> {
        self.task_mut(id).last_join_result.take()
    }
    pub(super) fn ipc_mut(&mut self, id: TaskId) -> &mut sched_ipc::TaskIpcState {
        &mut self.task_mut(id).ipc
    }
    pub(super) fn matches_blocked(&self, id: ThreadId, expected: BlockReason) -> bool {
        self.thread_matches(id)
            && self.task_state(TaskId::new(id.index())) == TaskState::Blocked(expected)
    }
    pub(super) fn replace_current_kind(
        &mut self,
        kind: ThreadKind,
    ) -> core::result::Result<(), ()> {
        let current = self.lifecycle.current.ok_or(())?;
        if !matches!(
            self.task_kind(TaskId::new(current.index())),
            ThreadKind::User { .. }
        ) {
            return Err(());
        }
        self.task_mut(TaskId::new(current.index())).kind = kind;
        Ok(())
    }

    fn validate_after_transition(&self) {
        #[cfg(any(debug_assertions, feature = "qemu-test"))]
        self.validate_invariants();
    }

    #[cfg(any(debug_assertions, feature = "qemu-test"))]
    pub(super) fn validate_invariants(&self) {
        if self.lifecycle.ready_queue.len() > self.lifecycle.ready_queue.capacity() {
            panic!("sched: ready queue exceeds capacity");
        }
        let mut running = 0usize;
        for (index, task) in self.lifecycle.tasks.iter().enumerate() {
            let id = TaskId::new(index);
            let tid = ThreadId::new(index, task.generation);
            let queued = self
                .lifecycle
                .ready_queue
                .iter()
                .filter(|queued| **queued == tid)
                .count();
            if queued > 1 {
                panic!("sched: duplicate ready entry {tid}");
            }
            match task.state {
                TaskState::Free => {
                    if task.context.is_some()
                        || task.joiner.is_some()
                        || task.joinable
                        || task.exit_code.is_some()
                        || task.last_join_result.is_some()
                        || !task.ipc.is_empty()
                        || task.kind != ThreadKind::Kernel
                        || queued != 0
                    {
                        panic!("sched: free slot {id} retains lifecycle metadata");
                    }
                }
                TaskState::Ready => {
                    if task.context.is_none() {
                        panic!("sched: ready slot {id} lacks context");
                    }
                    if id == IDLE_TASK_ID {
                        if queued != 0 {
                            panic!("sched: idle queued while ready");
                        }
                    } else if queued != 1 {
                        panic!("sched: ready slot {id} queue mismatch");
                    }
                }
                TaskState::Running => {
                    running += 1;
                    if task.context.is_none() || self.lifecycle.current != Some(tid) || queued != 0
                    {
                        panic!("sched: running slot {id} identity mismatch");
                    }
                }
                TaskState::Blocked(_) | TaskState::Zombie => {
                    if task.context.is_none() || queued != 0 {
                        panic!("sched: non-runnable slot {id} queue/context mismatch");
                    }
                }
            }
        }
        for queued in &self.lifecycle.ready_queue {
            if !self.thread_matches(*queued)
                || self.task_state(TaskId::new(queued.index())) != TaskState::Ready
                || queued.index() == IDLE_TASK_ID.index()
            {
                panic!("sched: stale ready queue entry {queued}");
            }
        }
        if self.lifecycle.current.is_none() {
            if running != 0 {
                panic!("sched: running task exists before initial dispatch");
            }
        } else if running != 1 {
            panic!("sched: expected exactly one running task");
        }
    }
}

fn next_generation(generation: u32) -> u32 {
    let next = generation.wrapping_add(1);
    if next == 0 { 1 } else { next }
}
