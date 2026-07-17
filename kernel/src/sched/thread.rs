use core::mem;

use crate::{
    arch::{ActiveContext, SavedContext},
    memory::vm::UserAddressSpace,
    process::ProcessId,
    sync::LocalIrqGuard,
    task::{TaskId, ThreadId},
    task_call::TaskCallWaitOutput,
};

use super::{
    CommitResult, IDLE_TASK_ID, Scheduler, WaitKind, scheduler_mut,
    transition::{SwitchOutcome, TaskState, ThreadKind},
    try_scheduler_mut,
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

/// Spawn a user thread by cloning a live userspace context.
///
/// The scheduler uses preallocated task/frame storage and keeps local IRQs
/// disabled only while publishing the child and invoking the saved-frame clone
/// hook. No allocation occurs in this function.
///
/// # Arguments
///
/// * `process_id` - Process identity assigned to the new user thread.
/// * `address_space` - Child TTBR0 address space already built by the process
///   layer.
/// * `context` - Exclusive live parent context used only as the clone source.
/// * `attrs` - Joinability attributes for the new scheduler task.
///
/// # Returns
///
/// Returns the generation-checked child thread ID.
///
/// # Errors
///
/// Returns [`SpawnError::SchedulerNotInitialized`] when scheduler state is not
/// published or [`SpawnError::NoThreadSlots`] when bounded capacity is full.
pub(crate) fn thread_spawn_user_from_context(
    process_id: ProcessId,
    address_space: UserAddressSpace,
    context: &mut ActiveContext<'_>,
    attrs: ThreadAttrs,
) -> core::result::Result<ThreadId, SpawnError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let Some(scheduler) = try_scheduler_mut() else {
        return Err(SpawnError::SchedulerNotInitialized);
    };

    scheduler.spawn_user_thread_from_context(process_id, address_space, context, attrs)
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

/// Terminate the current thread and replace its live return context.
///
/// # Arguments
///
/// * `context` - Exclusive live context replaced with the next runnable saved
///   frame.
/// * `code` - Thread exit result retained for join semantics when applicable.
///
/// # Returns
///
/// Returns only to the exception handler after frame replacement; the exiting
/// thread does not resume. This path does not allocate.
///
/// # Panics
///
/// Panics when no non-idle thread is currently running or a
/// [`crate::sync::preempt::PreemptGuard`] is active before terminal state changes.
pub(crate) fn on_thread_exit_sync(context: &mut ActiveContext<'_>, code: usize) {
    // Safe point: terminal handoff is permitted only after this enabled-state
    // assertion, before the exiting task's scheduler state is changed.
    crate::sync::preempt::assert_preemption_enabled("thread exit state change");
    scheduler_mut().exit_current(context, code);
}

/// Complete immediately or block the current thread joining `target`.
///
/// # Arguments
///
/// * `context` - Exclusive live task-call context used only if the join blocks.
/// * `target` - Generation-checked thread identity to join.
/// * `output` - Stack-owned task-call output that retains the exact join token
///   across a blocked resume.
///
/// # Returns
///
/// Returns after recording an immediate result or after the blocked joiner is
/// resumed. This bounded path does not allocate.
///
/// # Panics
///
/// Panics when scheduler running-state invariants are absent.
pub(crate) fn on_thread_join_sync(
    context: &mut ActiveContext<'_>,
    target: ThreadId,
    output: &mut TaskCallWaitOutput,
) {
    scheduler_mut().join_thread(context, target, output);
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
        let context = SavedContext::kernel_entry(
            self.stack_top(id),
            entry as *const () as usize,
            arg.as_usize(),
            thread_entry_bootstrap as *const () as usize,
        );

        let thread_id =
            self.transition_publish_runtime(id, context, ThreadKind::Kernel, attrs.joinable);

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
        let context = SavedContext::user_entry(user_entry, user_sp, self.stack_top(id), arg0);

        let thread_id = self.transition_publish_runtime(
            id,
            context,
            ThreadKind::User {
                process_id,
                address_space,
            },
            attrs.joinable,
        );

        crate::debug!(
            "thread: spawned user id={thread_id} pid={process_id} entry=0x{user_entry:x} sp=0x{user_sp:x} ttbr0=0x{:x}",
            address_space.root_pa()
        );
        Ok(thread_id)
    }

    fn spawn_user_thread_from_context(
        &mut self,
        process_id: ProcessId,
        address_space: UserAddressSpace,
        context: &mut ActiveContext<'_>,
        attrs: ThreadAttrs,
    ) -> core::result::Result<ThreadId, SpawnError> {
        let Some(id) = self.find_free_slot() else {
            return Err(SpawnError::NoThreadSlots);
        };
        let child_context = SavedContext::fork_child(context, self.stack_top(id));

        let thread_id = self.transition_publish_runtime(
            id,
            child_context,
            ThreadKind::User {
                process_id,
                address_space,
            },
            attrs.joinable,
        );

        crate::debug!(
            "thread: forked user id={thread_id} pid={process_id} ttbr0=0x{:x}",
            address_space.root_pa()
        );
        Ok(thread_id)
    }

    fn exit_current(&mut self, context: &mut ActiveContext<'_>, code: usize) {
        let SwitchOutcome::Switch {
            from: exited,
            to: next,
        } = self.transition_exit_current(code)
        else {
            panic!("thread: exit transition continued current task")
        };
        self.saved_context(next).restore_into(context);
        self.activate_task_address_space(next);
        self.finish_block_current(exited, next);
        crate::debug!("thread: exited id={exited} code={code}");
    }

    fn join_thread(
        &mut self,
        context: &mut ActiveContext<'_>,
        target: ThreadId,
        output: &mut TaskCallWaitOutput,
    ) {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("thread: join without running thread"));
        let current_thread = current;
        let _ = self.take_join_result(TaskId::new(current.index()));

        if current.index() == IDLE_TASK_ID.index() {
            self.set_join_result(TaskId::new(current.index()), Err(JoinError::NotJoinable));
            return;
        }

        let Some(target_task) = self.task_id_from_thread_id(target) else {
            self.set_join_result(TaskId::new(current.index()), Err(JoinError::InvalidThread));
            crate::debug!("thread: invalid/stale join target {target}");
            return;
        };

        if target_task == IDLE_TASK_ID {
            self.set_join_result(TaskId::new(current.index()), Err(JoinError::NotJoinable));
            return;
        }

        if target_task.index() == current.index() {
            self.set_join_result(TaskId::new(current.index()), Err(JoinError::SelfJoin));
            return;
        }

        if !self.task_joinable(target_task) {
            self.set_join_result(TaskId::new(current.index()), Err(JoinError::NotJoinable));
            return;
        }

        if self.task_joiner(target_task).is_some() {
            self.set_join_result(TaskId::new(current.index()), Err(JoinError::JoinInProgress));
            return;
        }

        if self.task_state(target_task) == TaskState::Zombie {
            let code = self
                .task_exit_code(target_task)
                .unwrap_or_else(|| panic!("thread: zombie without exit code"));
            self.transition_reap_zombie(target);
            self.set_join_result(TaskId::new(current.index()), Ok(code));
            crate::debug!("thread: join completed target={target} code={code}");
            return;
        }

        crate::sync::preempt::assert_preemption_enabled("thread joiner publication");
        let prepared = self.transition_prepare_wait(WaitKind::Thread);
        let token = prepared.token();
        output.record_token(token);
        self.task_set_joiner(target_task, token);
        match self.commit_prepared_wait(context, prepared) {
            CommitResult::Blocked(_) => {}
            CommitResult::Early(cause) => output.record_early(cause),
            CommitResult::Stale => panic!("thread: join wait became stale before commit"),
        }
        crate::debug!("thread: join blocking current={current_thread} target={target}");
    }

    fn running_thread_id(&self) -> Option<ThreadId> {
        self.current_thread()
    }

    fn task_id_from_thread_id(&self, id: ThreadId) -> Option<TaskId> {
        let task_id = TaskId::new(id.index());
        if !self.is_valid_task(task_id) {
            return None;
        }

        self.thread_matches(id).then_some(task_id)
    }

    fn take_current_join_result(&mut self) -> Option<core::result::Result<usize, JoinError>> {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("thread: join result without running thread"));
        self.take_join_result(TaskId::new(current.index()))
    }

    fn running_user_address_space(&self) -> Option<UserAddressSpace> {
        let current = self.running_task()?;
        match self.task_kind(TaskId::new(current.index())) {
            ThreadKind::User { address_space, .. } => Some(address_space),
            ThreadKind::Kernel => None,
        }
    }

    fn running_user_process_id(&self) -> Option<ProcessId> {
        let current = self.running_task()?;
        match self.task_kind(TaskId::new(current.index())) {
            ThreadKind::User { process_id, .. } => Some(process_id),
            ThreadKind::Kernel => None,
        }
    }

    fn replace_current_user_address_space(
        &mut self,
        address_space: UserAddressSpace,
    ) -> core::result::Result<(), ()> {
        let current = self.running_task().ok_or(())?;
        let process_id = match self.task_kind(TaskId::new(current.index())) {
            ThreadKind::User { process_id, .. } => process_id,
            ThreadKind::Kernel => return Err(()),
        };

        // SAFETY: execve commits a fully built TTBR0 root for the current user
        // process before replacing the trap frame that will resume to EL0.
        unsafe { crate::memory::vm::activate_user_address_space(address_space).map_err(|_| ())? };
        self.replace_current_kind(ThreadKind::User {
            process_id,
            address_space,
        })
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
