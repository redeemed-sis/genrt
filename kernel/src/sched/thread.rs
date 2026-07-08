use core::mem;

use crate::{
    memory::vm::UserAddressSpace,
    process::ProcessId,
    sync::LocalIrqGuard,
    task::{TaskId, ThreadId},
};

use super::{
    IDLE_TASK_ID, Scheduler, arch_clone_user_trap_frame_for_fork, arch_init_thread_frame,
    arch_init_user_trap_frame, copy_words, preempt, scheduler_mut, try_scheduler_mut,
};

#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ThreadArg(usize);

impl ThreadArg {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_usize(value: usize) -> Self {
        Self(value)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub fn from_ptr<T>(ptr: *const T) -> Self {
        Self(ptr as usize)
    }

    pub fn from_mut_ptr<T>(ptr: *mut T) -> Self {
        Self(ptr as usize)
    }

    pub const fn as_ptr<T>(self) -> *const T {
        self.0 as *const T
    }

    pub const fn as_mut_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }

    /// # Safety
    ///
    /// The caller must guarantee that the encoded pointer is valid for `T`,
    /// properly aligned, and remains live for the returned `'static` reference.
    pub unsafe fn as_static_ref<T>(self) -> Option<&'static T> {
        // SAFETY: upheld by the caller of this unsafe conversion.
        unsafe { self.as_ptr::<T>().as_ref() }
    }

    /// # Safety
    ///
    /// The caller must guarantee that the encoded pointer is uniquely owned,
    /// valid for `T`, properly aligned, and remains live for the returned
    /// `'static` mutable reference.
    pub unsafe fn as_static_mut<T>(self) -> Option<&'static mut T> {
        // SAFETY: upheld by the caller of this unsafe conversion.
        unsafe { self.as_mut_ptr::<T>().as_mut() }
    }
}

impl From<usize> for ThreadArg {
    fn from(value: usize) -> Self {
        Self::from_usize(value)
    }
}

impl<T> From<*const T> for ThreadArg {
    fn from(value: *const T) -> Self {
        Self::from_ptr(value)
    }
}

impl<T> From<*mut T> for ThreadArg {
    fn from(value: *mut T) -> Self {
        Self::from_mut_ptr(value)
    }
}

pub type ThreadEntry = fn(ThreadArg) -> usize;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ThreadAttrs {
    pub joinable: bool,
}

impl ThreadAttrs {
    pub const fn joinable() -> Self {
        Self { joinable: true }
    }

    pub const fn detached() -> Self {
        Self { joinable: false }
    }
}

impl Default for ThreadAttrs {
    fn default() -> Self {
        Self::joinable()
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpawnError {
    NoThreadSlots,
    NoStackSlots,
    SchedulerNotInitialized,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum JoinError {
    InvalidThread,
    NotJoinable,
    SelfJoin,
    JoinInProgress,
    SchedulerNotInitialized,
}

pub fn thread_spawn(
    entry: ThreadEntry,
    arg: ThreadArg,
    attrs: ThreadAttrs,
) -> core::result::Result<ThreadId, SpawnError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let Some(scheduler) = try_scheduler_mut() else {
        return Err(SpawnError::SchedulerNotInitialized);
    };

    scheduler.spawn_thread(entry, arg, attrs)
}

pub fn thread_exit(code: usize) -> ! {
    crate::task_call::thread_exit(code)
}

pub fn thread_join(id: ThreadId) -> core::result::Result<usize, JoinError> {
    {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        if try_scheduler_mut().is_none() {
            return Err(JoinError::SchedulerNotInitialized);
        }
    }

    crate::task_call::thread_join(id);
    take_current_join_result_irqsave()
}

pub fn current_thread_id() -> Option<ThreadId> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    try_scheduler_mut().and_then(|scheduler| scheduler.running_thread_id())
}

pub(crate) fn current_user_address_space() -> Option<UserAddressSpace> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    try_scheduler_mut().and_then(|scheduler| scheduler.running_user_address_space())
}

pub(crate) fn current_user_process_id() -> Option<ProcessId> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    try_scheduler_mut().and_then(|scheduler| scheduler.running_user_process_id())
}

pub(crate) fn thread_spawn_user(
    process_id: ProcessId,
    address_space: UserAddressSpace,
    user_entry: usize,
    user_sp: usize,
    arg0: usize,
    attrs: ThreadAttrs,
) -> core::result::Result<ThreadId, SpawnError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let Some(scheduler) = try_scheduler_mut() else {
        return Err(SpawnError::SchedulerNotInitialized);
    };

    scheduler.spawn_user_thread(process_id, address_space, user_entry, user_sp, arg0, attrs)
}

pub(crate) fn thread_spawn_user_from_frame(
    process_id: ProcessId,
    address_space: UserAddressSpace,
    frame_words: *const u64,
    attrs: ThreadAttrs,
) -> core::result::Result<ThreadId, SpawnError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let Some(scheduler) = try_scheduler_mut() else {
        return Err(SpawnError::SchedulerNotInitialized);
    };

    scheduler.spawn_user_thread_from_frame(process_id, address_space, frame_words, attrs)
}

pub(crate) fn replace_current_user_address_space(
    address_space: UserAddressSpace,
) -> core::result::Result<(), ()> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let Some(scheduler) = try_scheduler_mut() else {
        return Err(());
    };

    scheduler.replace_current_user_address_space(address_space)
}

pub(crate) fn on_thread_exit_sync(active_frame_words: *mut u64, code: usize) {
    scheduler_mut().exit_current(active_frame_words, code);
}

pub(crate) fn on_thread_join_sync(active_frame_words: *mut u64, target: ThreadId) {
    scheduler_mut().join_thread(active_frame_words, target);
}

pub(crate) fn block_current_on_process_wait(active_frame_words: *mut u64, pid: ProcessId) {
    scheduler_mut().block_current_on_process_wait(active_frame_words, pid);
}

pub(crate) fn complete_process_wait(waiter: ThreadId, pid: ProcessId) {
    scheduler_mut().complete_process_wait(waiter, pid);
}

pub(crate) fn block_current_on_stdin_read(active_frame_words: *mut u64) {
    scheduler_mut().block_current_on_stdin_read(active_frame_words);
}

pub(crate) fn complete_stdin_read(waiter: ThreadId) {
    scheduler_mut().complete_stdin_read(waiter);
}

enum BlockedWaiterState {
    Missing,
    WrongState(preempt::TaskState),
}

impl Scheduler {
    fn spawn_thread(
        &mut self,
        entry: ThreadEntry,
        arg: ThreadArg,
        attrs: ThreadAttrs,
    ) -> core::result::Result<ThreadId, SpawnError> {
        let Some(id) = self.find_free_slot() else {
            return Err(SpawnError::NoThreadSlots);
        };

        let thread_id = {
            let task = self.task_mut(id);
            task.generation = next_generation(task.generation);
            task.state = preempt::TaskState::Ready;
            task.joinable = attrs.joinable;
            task.exit_code = None;
            task.joiner = None;
            task.ipc.reset();
            task.last_join_result = None;
            task.entry = free_task_entry;
            task.arg = ThreadArg::empty();
            task.kind = preempt::ThreadKind::Kernel;
            ThreadId::new(id.index(), task.generation)
        };

        self.initialize_spawned_thread_frame(id, entry, arg);
        if id != IDLE_TASK_ID {
            let was_empty = self.ready_queue.is_empty();
            self.ready_push_back(id);
            if was_empty {
                self.note_runnable_peer_available();
            }
        }

        crate::debug!(
            "thread: spawned id={thread_id} joinable={} arg={}",
            attrs.joinable,
            arg.as_usize()
        );
        Ok(thread_id)
    }

    fn spawn_user_thread(
        &mut self,
        process_id: ProcessId,
        address_space: UserAddressSpace,
        user_entry: usize,
        user_sp: usize,
        arg0: usize,
        attrs: ThreadAttrs,
    ) -> core::result::Result<ThreadId, SpawnError> {
        let Some(id) = self.find_free_slot() else {
            return Err(SpawnError::NoThreadSlots);
        };

        let thread_id = {
            let task = self.task_mut(id);
            task.generation = next_generation(task.generation);
            task.state = preempt::TaskState::Ready;
            task.joinable = attrs.joinable;
            task.exit_code = None;
            task.joiner = None;
            task.ipc.reset();
            task.last_join_result = None;
            task.entry = free_task_entry;
            task.arg = ThreadArg::empty();
            task.kind = preempt::ThreadKind::User {
                process_id,
                address_space,
            };
            ThreadId::new(id.index(), task.generation)
        };

        self.initialize_spawned_user_thread_frame(id, user_entry, user_sp, arg0);
        if id != IDLE_TASK_ID {
            let was_empty = self.ready_queue.is_empty();
            self.ready_push_back(id);
            if was_empty {
                self.note_runnable_peer_available();
            }
        }

        crate::debug!(
            "thread: spawned user id={thread_id} pid={process_id} entry=0x{user_entry:x} sp=0x{user_sp:x} ttbr0=0x{:x}",
            address_space.root_pa()
        );
        Ok(thread_id)
    }

    fn initialize_spawned_thread_frame(&mut self, id: TaskId, entry: ThreadEntry, arg: ThreadArg) {
        let stack_top = self.task(id).stack_top();
        let frame_words = self.frame_words_mut_ptr(id);
        // SAFETY: scheduler owns stable frame and stack storage for this slot.
        unsafe {
            arch_init_thread_frame(
                frame_words,
                stack_top,
                entry as *const () as usize,
                arg.as_usize(),
                thread_entry_bootstrap as *const () as usize,
            );
        }
    }

    fn initialize_spawned_user_thread_frame(
        &mut self,
        id: TaskId,
        user_entry: usize,
        user_sp: usize,
        arg0: usize,
    ) {
        let kernel_sp = self.task(id).stack_top();
        let frame_words = self.frame_words_mut_ptr(id);
        // SAFETY: scheduler owns stable frame and kernel stack storage. The
        // process layer already mapped `user_entry` and `user_sp` in TTBR0.
        unsafe {
            arch_init_user_trap_frame(frame_words, user_entry, user_sp, kernel_sp, arg0);
        }
    }

    fn spawn_user_thread_from_frame(
        &mut self,
        process_id: ProcessId,
        address_space: UserAddressSpace,
        source_frame_words: *const u64,
        attrs: ThreadAttrs,
    ) -> core::result::Result<ThreadId, SpawnError> {
        if source_frame_words.is_null() {
            return Err(SpawnError::NoThreadSlots);
        }

        let Some(id) = self.find_free_slot() else {
            return Err(SpawnError::NoThreadSlots);
        };

        let thread_id = {
            let task = self.task_mut(id);
            task.generation = next_generation(task.generation);
            task.state = preempt::TaskState::Ready;
            task.joinable = attrs.joinable;
            task.exit_code = None;
            task.joiner = None;
            task.ipc.reset();
            task.last_join_result = None;
            task.entry = free_task_entry;
            task.arg = ThreadArg::empty();
            task.kind = preempt::ThreadKind::User {
                process_id,
                address_space,
            };
            ThreadId::new(id.index(), task.generation)
        };

        let kernel_sp = self.task(id).stack_top();
        // SAFETY: scheduler owns the destination frame and child kernel stack.
        // The source frame is the live lower-EL syscall frame supplied by the
        // caller. The arch helper copies the userspace resume state and fixes
        // the child-only return value/kernel stack.
        unsafe {
            arch_clone_user_trap_frame_for_fork(
                self.frame_words_mut_ptr(id),
                source_frame_words,
                kernel_sp,
            );
        }

        if id != IDLE_TASK_ID {
            let was_empty = self.ready_queue.is_empty();
            self.ready_push_back(id);
            if was_empty {
                self.note_runnable_peer_available();
            }
        }

        crate::debug!(
            "thread: forked user id={thread_id} pid={process_id} ttbr0=0x{:x}",
            address_space.root_pa()
        );
        Ok(thread_id)
    }

    fn find_free_slot(&self) -> Option<TaskId> {
        self.tasks
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(index, task)| task.is_free().then_some(TaskId::new(index)))
    }

    fn exit_current(&mut self, active_frame_words: *mut u64, code: usize) {
        if active_frame_words.is_null() {
            panic!("thread: exit without active frame");
        }

        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("thread: exit without running thread"));
        if current == IDLE_TASK_ID {
            panic!("thread: idle thread cannot exit");
        }

        let exited = self.thread_id(current);
        self.finish_current_exit(current, exited, code);

        let next = self.dequeue_next_ready().unwrap_or(IDLE_TASK_ID);
        copy_words(active_frame_words, self.frame_words_ptr(next));
        self.make_running(next);
        self.finish_block_current(current, next);
        crate::debug!("thread: exited id={exited} code={code}");
    }

    fn join_thread(&mut self, active_frame_words: *mut u64, target: ThreadId) {
        if active_frame_words.is_null() {
            panic!("thread: join without active frame");
        }

        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("thread: join without running thread"));
        let current_thread = self.thread_id(current);
        self.task_mut(current).last_join_result = None;

        if current == IDLE_TASK_ID {
            self.finish_join_immediate(current, Err(JoinError::NotJoinable));
            return;
        }

        let Some(target_task) = self.task_id_from_thread_id(target) else {
            self.finish_join_immediate(current, Err(JoinError::InvalidThread));
            crate::debug!("thread: invalid/stale join target {target}");
            return;
        };

        if target_task == IDLE_TASK_ID {
            self.finish_join_immediate(current, Err(JoinError::NotJoinable));
            return;
        }

        if target_task == current {
            self.finish_join_immediate(current, Err(JoinError::SelfJoin));
            return;
        }

        if !self.task(target_task).joinable {
            self.finish_join_immediate(current, Err(JoinError::NotJoinable));
            return;
        }

        if self.task(target_task).joiner.is_some() {
            self.finish_join_immediate(current, Err(JoinError::JoinInProgress));
            return;
        }

        if self.task(target_task).state == preempt::TaskState::Zombie {
            let code = self
                .task(target_task)
                .exit_code
                .unwrap_or_else(|| panic!("thread: zombie without exit code"));
            self.reclaim_thread(target_task);
            self.finish_join_immediate(current, Ok(code));
            crate::debug!("thread: join completed target={target} code={code}");
            return;
        }

        self.task_mut(target_task).joiner = Some(current_thread);
        let next = self.block_current_with_reason(
            active_frame_words,
            current,
            preempt::BlockReason::Join(target),
        );
        self.finish_block_current(current, next);
        crate::debug!("thread: join blocking current={current_thread} target={target}");
    }

    fn running_thread_id(&self) -> Option<ThreadId> {
        self.current.map(|id| self.thread_id(id))
    }

    fn thread_id(&self, id: TaskId) -> ThreadId {
        ThreadId::new(id.index(), self.task(id).generation)
    }

    fn task_id_from_thread_id(&self, id: ThreadId) -> Option<TaskId> {
        let task_id = TaskId::new(id.index());
        if !self.is_valid_task(task_id) {
            return None;
        }

        let task = self.task(task_id);
        (task.generation == id.generation() && !task.is_free()).then_some(task_id)
    }

    fn finish_current_exit(&mut self, current: TaskId, exited: ThreadId, code: usize) {
        let joinable = self.task(current).joinable;
        let joiner = self.task(current).joiner;

        self.task_mut(current).exit_code = Some(code);

        if let Some(joiner) = joiner {
            self.task_mut(current).state = preempt::TaskState::Zombie;
            self.complete_joiner(exited, joiner, code);
            self.reclaim_thread(current);
            return;
        }

        if joinable {
            self.task_mut(current).state = preempt::TaskState::Zombie;
        } else {
            self.reclaim_thread(current);
        }
    }

    fn complete_joiner(&mut self, target: ThreadId, joiner: ThreadId, code: usize) {
        let joiner_task = match self.blocked_waiter(joiner, preempt::BlockReason::Join(target)) {
            Ok(task_id) => task_id,
            Err(BlockedWaiterState::Missing) => {
                panic!("thread: joiner {joiner} disappeared while target {target} exited")
            }
            Err(BlockedWaiterState::WrongState(state)) => {
                panic!("thread: joiner {joiner} has invalid state {state:?}")
            }
        };
        self.task_mut(joiner_task).last_join_result = Some(Ok(code));
        self.make_ready_and_queue(joiner_task);
        crate::debug!("thread: join wake target={target} joiner={joiner} code={code}");
    }

    fn finish_join_immediate(
        &mut self,
        current: TaskId,
        result: core::result::Result<usize, JoinError>,
    ) {
        self.task_mut(current).last_join_result = Some(result);
    }

    fn reclaim_thread(&mut self, id: TaskId) {
        if id == IDLE_TASK_ID {
            panic!("thread: idle thread cannot be reclaimed");
        }

        debug_assert!(
            !self.ready_queue.iter().any(|queued| *queued == id),
            "thread: reclaim target still queued ready"
        );

        let generation = self.task(id).generation;
        let task = self.task_mut(id);
        task.state = preempt::TaskState::Free;
        task.joinable = false;
        task.exit_code = None;
        task.joiner = None;
        task.ipc.reset();
        task.last_join_result = None;
        task.entry = free_task_entry;
        task.arg = ThreadArg::empty();
        task.kind = preempt::ThreadKind::Kernel;
        crate::debug!(
            "thread: reclaimed slot={} generation={generation}",
            id.index()
        );
    }

    fn take_current_join_result(&mut self) -> Option<core::result::Result<usize, JoinError>> {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("thread: join result without running thread"));
        self.task_mut(current).last_join_result.take()
    }

    fn running_user_address_space(&self) -> Option<UserAddressSpace> {
        let current = self.running_task()?;
        match self.task(current).kind {
            preempt::ThreadKind::User { address_space, .. } => Some(address_space),
            preempt::ThreadKind::Kernel => None,
        }
    }

    fn running_user_process_id(&self) -> Option<ProcessId> {
        let current = self.running_task()?;
        match self.task(current).kind {
            preempt::ThreadKind::User { process_id, .. } => Some(process_id),
            preempt::ThreadKind::Kernel => None,
        }
    }

    fn replace_current_user_address_space(
        &mut self,
        address_space: UserAddressSpace,
    ) -> core::result::Result<(), ()> {
        let current = self.running_task().ok_or(())?;
        let process_id = match self.task(current).kind {
            preempt::ThreadKind::User { process_id, .. } => process_id,
            preempt::ThreadKind::Kernel => return Err(()),
        };

        // SAFETY: execve commits a fully built TTBR0 root for the current user
        // process before replacing the trap frame that will resume to EL0.
        unsafe { crate::memory::vm::activate_user_address_space(address_space).map_err(|_| ())? };
        self.task_mut(current).kind = preempt::ThreadKind::User {
            process_id,
            address_space,
        };
        Ok(())
    }

    fn block_current_on_process_wait(&mut self, active_frame_words: *mut u64, pid: ProcessId) {
        let current = self.blocking_current(active_frame_words);
        let next = self.block_current_with_reason(
            active_frame_words,
            current,
            preempt::BlockReason::Process(pid),
        );
        self.finish_block_current(current, next);
    }

    fn block_current_on_stdin_read(&mut self, active_frame_words: *mut u64) {
        let current = self.blocking_current(active_frame_words);
        let next = self.block_current_with_reason(
            active_frame_words,
            current,
            preempt::BlockReason::StdinRead,
        );
        self.finish_block_current(current, next);
    }

    fn complete_process_wait(&mut self, waiter: ThreadId, pid: ProcessId) {
        match self.blocked_waiter(waiter, preempt::BlockReason::Process(pid)) {
            Ok(task_id) => {
                self.make_ready_and_queue(task_id);
                crate::debug!("process: wake pid={pid} waiter={waiter}");
            }
            Err(BlockedWaiterState::Missing) => {
                crate::trace!("process: waiter {waiter} disappeared while pid {pid} exited");
            }
            Err(BlockedWaiterState::WrongState(state)) => {
                crate::trace!("process: ignoring wake for waiter {waiter}; state={state:?}");
            }
        }
    }

    fn complete_stdin_read(&mut self, waiter: ThreadId) {
        match self.blocked_waiter(waiter, preempt::BlockReason::StdinRead) {
            Ok(task_id) => {
                self.make_ready_and_queue(task_id);
                crate::trace!("stdin: wake waiter={waiter}");
            }
            Err(BlockedWaiterState::Missing) => {
                crate::trace!("stdin: waiter {waiter} disappeared before UART wake");
            }
            Err(BlockedWaiterState::WrongState(state)) => {
                crate::trace!("stdin: ignoring wake for waiter {waiter}; state={state:?}");
            }
        }
    }

    fn blocked_waiter(
        &self,
        waiter: ThreadId,
        expected: preempt::BlockReason,
    ) -> core::result::Result<TaskId, BlockedWaiterState> {
        let Some(task_id) = self.task_id_from_thread_id(waiter) else {
            return Err(BlockedWaiterState::Missing);
        };

        match self.task(task_id).state {
            preempt::TaskState::Blocked(reason) if reason == expected => Ok(task_id),
            state => Err(BlockedWaiterState::WrongState(state)),
        }
    }
}

fn take_current_join_result_irqsave() -> core::result::Result<usize, JoinError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    scheduler_mut()
        .take_current_join_result()
        .unwrap_or(Err(JoinError::InvalidThread))
}

pub(super) extern "C" fn thread_entry_bootstrap(entry_addr: usize, arg: usize) -> ! {
    // SAFETY: frame setup passes a `ThreadEntry` function pointer in x0 and a
    // raw `ThreadArg` payload in x1.
    let entry_fn: ThreadEntry = unsafe { mem::transmute(entry_addr) };
    let code = entry_fn(ThreadArg::from_usize(arg));
    thread_exit(code)
}

pub(super) fn free_task_entry(_arg: ThreadArg) -> usize {
    panic!("thread: free thread slot entered");
}

fn next_generation(generation: u32) -> u32 {
    let next = generation.wrapping_add(1);
    if next == 0 { 1 } else { next }
}
