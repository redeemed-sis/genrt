use core::mem;

use crate::{
    arch::{ActiveContext, SavedContext},
    cpu::{self, CpuId},
    memory::{user::OwnedUserStack, vm::AddressSpaceId},
    sync::LocalIrqGuard,
};

use super::{
    CommitResult, CpuScheduler, ThreadId,
    call::SchedCallWaitOutput,
    current_cpu, scheduler_mut,
    transition::{SwitchOutcome, ThreadState, UserThreadResources},
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
/// Immutable thread publication attributes.
///
/// Attributes are consumed before a thread becomes visible. Changing this
/// value later cannot migrate or otherwise modify a published thread.
pub struct ThreadAttrs {
    /// Whether one waiter may later consume this thread's exit status.
    pub joinable: bool,
    /// CPU placement resolved once before scheduler publication.
    pub affinity: ThreadAffinity,
}

/// Immutable placement selected before a thread is published.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ThreadAffinity {
    /// Place the thread on the CPU that performs publication.
    Current,
    /// Place the thread on one already registered and online logical CPU.
    Cpu(CpuId),
}

impl ThreadAttrs {
    /// Construct joinable attributes with current-CPU affinity.
    ///
    /// # Returns
    ///
    /// Returns immutable publication metadata without allocation, blocking,
    /// or IRQ-state changes.
    pub const fn joinable() -> Self {
        Self {
            joinable: true,
            affinity: ThreadAffinity::Current,
        }
    }

    /// Construct detached attributes with current-CPU affinity.
    ///
    /// # Returns
    ///
    /// Returns immutable publication metadata without allocation, blocking,
    /// or IRQ-state changes.
    pub const fn detached() -> Self {
        Self {
            joinable: false,
            affinity: ThreadAffinity::Current,
        }
    }

    /// Select an immutable logical CPU affinity before publication.
    ///
    /// # Arguments
    ///
    /// * `cpu` - Registered logical CPU that must be online when spawning.
    ///
    /// # Returns
    ///
    /// Returns modified attributes without allocation, blocking, or IRQ-state
    /// changes. The selected CPU becomes the thread's permanent home.
    pub const fn with_affinity(mut self, cpu: CpuId) -> Self {
        self.affinity = ThreadAffinity::Cpu(cpu);
        self
    }
}

impl Default for ThreadAttrs {
    fn default() -> Self {
        Self::joinable()
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
/// Controlled failures while publishing a thread into bounded scheduler state.
pub enum SpawnError {
    /// No preallocated thread slot remains free.
    NoThreadSlots,
    /// A preallocated slot unexpectedly has no parked kernel stack.
    NoStackSlots,
    /// Scheduler bootstrap has not published runtime state.
    SchedulerNotInitialized,
    /// The requested logical CPU is not registered or outside scheduler storage.
    InvalidCpuAffinity,
    /// The requested registered logical CPU is not online.
    CpuOffline,
    /// User threads retain non-Copy resources and therefore must have a
    /// generic reaper; detached operation is reserved for kernel threads.
    UserThreadMustBeJoinable,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum JoinError {
    InvalidThread,
    NotJoinable,
    SelfJoin,
    JoinInProgress,
    SchedulerNotInitialized,
}

/// Publish a kernel thread into bounded scheduler storage.
///
/// The current logical CPU is resolved once before short local IRQ exclusion.
/// Publication uses a preallocated slot and stack and does not allocate or
/// block. The thread's resolved home CPU is immutable.
///
/// # Arguments
///
/// * `entry` - Function invoked when the new thread first runs.
/// * `arg` - Opaque argument passed to `entry`.
/// * `attrs` - Joinability and CPU affinity fixed before publication.
///
/// # Returns
///
/// Returns the generation-bearing thread identity after ready publication.
///
/// # Errors
///
/// Returns [`SpawnError::SchedulerNotInitialized`] before bootstrap,
/// [`SpawnError::NoThreadSlots`] when bounded capacity is exhausted,
/// [`SpawnError::InvalidCpuAffinity`] for an unregistered CPU, or
/// [`SpawnError::CpuOffline`] for a registered context not yet online.
pub fn thread_spawn(
    entry: ThreadEntry,
    arg: ThreadArg,
    attrs: ThreadAttrs,
) -> core::result::Result<ThreadId, SpawnError> {
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let Some(mut scheduler) = try_scheduler_mut() else {
        return Err(SpawnError::SchedulerNotInitialized);
    };

    scheduler.on_cpu(cpu).spawn_thread(entry, arg, attrs)
}

pub fn thread_exit(code: usize) -> ! {
    crate::sched::call::thread_exit(code)
}

pub fn thread_join(id: ThreadId) -> core::result::Result<usize, JoinError> {
    {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        if try_scheduler_mut().is_none() {
            return Err(JoinError::SchedulerNotInitialized);
        }
    }

    crate::sched::call::thread_join(id);
    let (result, reaped_user) = take_current_join_result_irqsave();
    drop(reaped_user);
    result
}

/// Return the running thread on the executing logical CPU.
///
/// The function resolves the CPU once, performs a bounded lookup under short
/// local IRQ exclusion, and neither allocates nor blocks.
///
/// # Returns
///
/// Returns the current generation-bearing thread identity, or `None` before
/// scheduler publication or initial dispatch.
pub fn current_thread_id() -> Option<ThreadId> {
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    try_scheduler_mut().and_then(|mut scheduler| scheduler.on_cpu(cpu).running_thread_id())
}

/// Return the active user thread's copyable address-space identifier.
///
/// The scheduler performs a bounded lookup under local IRQ exclusion without
/// allocating or blocking.
///
/// # Returns
///
/// Returns `Some` only while the current schedulable thread owns userspace
/// resources; kernel threads return `None`.
pub(crate) fn current_user_address_space() -> Option<AddressSpaceId> {
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    try_scheduler_mut().and_then(|mut scheduler| scheduler.on_cpu(cpu).running_user_address_space())
}

/// Return a raw pointer to the current user thread's owned stack.
///
/// This performs a bounded, allocation-free lookup under local IRQ exclusion.
/// It does not block or enter the scheduler.
///
/// # Safety
///
/// The caller must ensure the current thread cannot exit or be reaped while
/// dereferencing the returned pointer. The pointer must not be retained across
/// a lifecycle transition.
///
/// # Returns
///
/// Returns `Some` for a current user thread and `None` for a kernel thread or
/// before scheduler initialization.
pub(crate) unsafe fn current_user_stack_ptr() -> Option<*const OwnedUserStack> {
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    try_scheduler_mut().and_then(|mut scheduler| {
        let current = scheduler.on_cpu(cpu).running_thread_id()?;
        scheduler
            .thread_user_stack(current.index())
            .map(|stack| stack as *const OwnedUserStack)
    })
}

/// Publish a user thread that consumes a prepared mapped user stack.
///
/// Publication runs under short local IRQ exclusion, performs no allocation,
/// and moves `stack` into the thread only after a preallocated slot is found.
///
/// # Arguments
///
/// * `address_space` - Copyable ID for the already-owned process root.
/// * `stack` - Unique mapped user stack consumed only on successful publication.
/// * `user_entry` - Initial EL0 program counter.
/// * `user_sp` - Initial EL0 stack pointer within `stack`.
/// * `arg0` - Initial EL0 argument register value.
/// * `attrs` - Joinability selected for the new thread.
///
/// # Returns
///
/// Returns the generation-bearing thread ID on success. On error, returns both
/// the spawn error and unchanged stack for cleanup outside IRQ exclusion.
///
/// # Errors
///
/// Returns [`SpawnError::NoThreadSlots`] for exhausted bounded capacity or
/// [`SpawnError::SchedulerNotInitialized`] before bootstrap publication.
/// [`SpawnError::UserThreadMustBeJoinable`] rejects detached userspace because
/// its non-Copy stack must be released by generic join/reap.
pub(crate) fn thread_spawn_user(
    address_space: AddressSpaceId,
    stack: OwnedUserStack,
    user_entry: usize,
    user_sp: usize,
    arg0: usize,
    attrs: ThreadAttrs,
) -> core::result::Result<ThreadId, (SpawnError, OwnedUserStack)> {
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let Some(mut scheduler) = try_scheduler_mut() else {
        return Err((SpawnError::SchedulerNotInitialized, stack));
    };

    scheduler
        .on_cpu(cpu)
        .spawn_user_thread(address_space, stack, user_entry, user_sp, arg0, attrs)
}

/// Spawn a user thread by cloning a live userspace context.
///
/// The scheduler uses preallocated thread/frame storage and keeps local IRQs
/// disabled only while publishing the child and invoking the saved-frame clone
/// hook. No allocation occurs in this function.
///
/// # Arguments
///
/// * `address_space` - Child TTBR0 address space already built by the process
///   layer.
/// * `stack` - Child mapped user stack consumed only when publication succeeds.
/// * `context` - Exclusive live parent context used only as the clone source.
/// * `attrs` - Joinability attributes for the new scheduler thread.
///
/// # Returns
///
/// Returns the generation-checked child thread ID. On error, returns the
/// unchanged stack with the spawn error for cleanup outside IRQ exclusion.
///
/// # Errors
///
/// Returns [`SpawnError::SchedulerNotInitialized`] when scheduler state is not
/// published, [`SpawnError::NoThreadSlots`] when bounded capacity is full, or
/// [`SpawnError::UserThreadMustBeJoinable`] for detached userspace.
pub(crate) fn thread_spawn_user_from_context(
    address_space: AddressSpaceId,
    stack: OwnedUserStack,
    context: &mut ActiveContext<'_>,
    attrs: ThreadAttrs,
) -> core::result::Result<ThreadId, (SpawnError, OwnedUserStack)> {
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let Some(mut scheduler) = try_scheduler_mut() else {
        return Err((SpawnError::SchedulerNotInitialized, stack));
    };

    scheduler
        .on_cpu(cpu)
        .spawn_user_thread_from_context(address_space, stack, context, attrs)
}

/// Activate and replace the current user thread's owned userspace resources.
///
/// This is the scheduler half of exec publication. It activates `address_space`
/// before an infallible payload exchange under short local IRQ exclusion; it
/// neither allocates nor blocks.
///
/// # Arguments
///
/// * `address_space` - Prevalidated ID of the staged owned address space.
/// * `stack` - Staged mapped user stack consumed on a successful exchange.
///
/// # Returns
///
/// Returns the old thread resources, whose stack must be dropped after the
/// caller's IRQ guard. On error, returns the unchanged staged stack.
///
/// # Errors
///
/// Returns an error when no current user thread exists or architecture address
/// space activation rejects the staged root.
pub(crate) fn replace_current_user_resources(
    address_space: AddressSpaceId,
    stack: OwnedUserStack,
) -> core::result::Result<UserThreadResources, ((), OwnedUserStack)> {
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let Some(mut scheduler) = try_scheduler_mut() else {
        return Err(((), stack));
    };

    scheduler
        .on_cpu(cpu)
        .replace_current_user_resources(address_space, stack)
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
    let cpu = current_cpu();
    // Safe point: terminal handoff is permitted only after this enabled-state
    // assertion, before the exiting thread's scheduler state is changed.
    crate::sync::preempt::assert_preemption_enabled_on(cpu, "thread exit state change");
    scheduler_mut().on_cpu(cpu).exit_current(context, code);
}

/// Complete immediately or block the current thread joining `target`.
///
/// # Arguments
///
/// * `context` - Exclusive live sched-call context used only if the join blocks.
/// * `target` - Generation-checked thread identity to join.
/// * `output` - Stack-owned sched-call output that retains the exact join token
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
    output: &mut SchedCallWaitOutput,
) {
    let cpu = current_cpu();
    scheduler_mut()
        .on_cpu(cpu)
        .join_thread(context, target, output);
}

impl CpuScheduler<'_> {
    fn resolve_affinity(
        &self,
        affinity: ThreadAffinity,
    ) -> core::result::Result<CpuId, SpawnError> {
        let target = match affinity {
            ThreadAffinity::Current => self.cpu(),
            ThreadAffinity::Cpu(cpu) => cpu,
        };
        if target.index() >= self.cpus.len() || !cpu::is_registered(target) {
            return Err(SpawnError::InvalidCpuAffinity);
        }
        if !self.cpu_state_for(target).online {
            return Err(SpawnError::CpuOffline);
        }
        Ok(target)
    }

    fn spawn_thread(
        &mut self,
        entry: ThreadEntry,
        arg: ThreadArg,
        attrs: ThreadAttrs,
    ) -> core::result::Result<ThreadId, SpawnError> {
        let target_cpu = self.resolve_affinity(attrs.affinity)?;
        let Some(id) = self.take_free_slot() else {
            return Err(SpawnError::NoThreadSlots);
        };
        let context = SavedContext::kernel_entry(
            self.stack_top(id),
            entry as *const () as usize,
            arg.as_usize(),
            thread_entry_bootstrap as *const () as usize,
        );

        let thread_id =
            self.transition_publish_runtime(target_cpu, id, context, None, attrs.joinable);

        crate::debug!(
            "thread: spawned id={thread_id} joinable={} arg={}",
            attrs.joinable,
            arg.as_usize()
        );
        Ok(thread_id)
    }

    fn spawn_user_thread(
        &mut self,
        address_space: AddressSpaceId,
        stack: OwnedUserStack,
        user_entry: usize,
        user_sp: usize,
        arg0: usize,
        attrs: ThreadAttrs,
    ) -> core::result::Result<ThreadId, (SpawnError, OwnedUserStack)> {
        let target_cpu = match self.resolve_affinity(attrs.affinity) {
            Ok(cpu) => cpu,
            Err(error) => return Err((error, stack)),
        };
        if !attrs.joinable {
            return Err((SpawnError::UserThreadMustBeJoinable, stack));
        }
        let Some(id) = self.take_free_slot() else {
            return Err((SpawnError::NoThreadSlots, stack));
        };
        let context = SavedContext::user_entry(user_entry, user_sp, self.stack_top(id), arg0);

        let thread_id = self.transition_publish_runtime(
            target_cpu,
            id,
            context,
            Some(UserThreadResources::new(address_space, stack)),
            attrs.joinable,
        );

        crate::debug!(
            "thread: spawned user id={thread_id} entry=0x{user_entry:x} sp=0x{user_sp:x}",
        );
        Ok(thread_id)
    }

    fn spawn_user_thread_from_context(
        &mut self,
        address_space: AddressSpaceId,
        stack: OwnedUserStack,
        context: &mut ActiveContext<'_>,
        attrs: ThreadAttrs,
    ) -> core::result::Result<ThreadId, (SpawnError, OwnedUserStack)> {
        let target_cpu = match self.resolve_affinity(attrs.affinity) {
            Ok(cpu) => cpu,
            Err(error) => return Err((error, stack)),
        };
        if !attrs.joinable {
            return Err((SpawnError::UserThreadMustBeJoinable, stack));
        }
        let Some(id) = self.take_free_slot() else {
            return Err((SpawnError::NoThreadSlots, stack));
        };
        let child_context = SavedContext::fork_child(context, self.stack_top(id));

        let thread_id = self.transition_publish_runtime(
            target_cpu,
            id,
            child_context,
            Some(UserThreadResources::new(address_space, stack)),
            attrs.joinable,
        );

        crate::debug!("thread: forked user id={thread_id}",);
        Ok(thread_id)
    }

    fn exit_current(&mut self, context: &mut ActiveContext<'_>, code: usize) {
        let SwitchOutcome::Switch {
            from: exited,
            to: next,
        } = self.transition_exit_current(code)
        else {
            panic!("thread: exit transition continued current thread")
        };
        self.saved_context(next).restore_into(context);
        self.activate_thread_address_space(next);
        self.finish_block_current(exited, next);
        crate::debug!("thread: exited id={exited} code={code}");
    }

    fn join_thread(
        &mut self,
        context: &mut ActiveContext<'_>,
        target: ThreadId,
        output: &mut SchedCallWaitOutput,
    ) {
        let current = self
            .running_thread()
            .unwrap_or_else(|| panic!("thread: join without running thread"));
        let current_thread = current;
        if self.has_join_result(current.index()) || self.has_reaped_user(current.index()) {
            panic!("thread: join entered with an unconsumed prior join handoff");
        }

        if self.state().idle == Some(current) {
            self.set_join_result(current.index(), Err(JoinError::NotJoinable));
            return;
        }

        if !self.thread_matches(target) {
            self.set_join_result(current.index(), Err(JoinError::InvalidThread));
            crate::debug!("thread: invalid/stale join target {target}");
            return;
        }
        let target_slot = target.index();

        if self.cpu_state_for(self.thread_home_cpu(target_slot)).idle == Some(target) {
            self.set_join_result(current.index(), Err(JoinError::NotJoinable));
            return;
        }

        if target_slot == current.index() {
            self.set_join_result(current.index(), Err(JoinError::SelfJoin));
            return;
        }

        if !self.thread_joinable(target_slot) {
            self.set_join_result(current.index(), Err(JoinError::NotJoinable));
            return;
        }

        if self.thread_joiner(target_slot).is_some() {
            self.set_join_result(current.index(), Err(JoinError::JoinInProgress));
            return;
        }

        if self.thread_state(target_slot) == ThreadState::Exited {
            let code = self
                .thread_exit_code(target_slot)
                .unwrap_or_else(|| panic!("thread: exited thread without exit code"));
            let reaped = self.transition_reap_exited(target);
            self.set_reaped_user(current, reaped);
            self.set_join_result(current.index(), Ok(code));
            crate::debug!("thread: join completed target={target} code={code}");
            return;
        }

        crate::sync::preempt::assert_preemption_enabled_on(self.cpu(), "thread joiner publication");
        let prepared = self.transition_prepare_wait();
        let token = prepared.token();
        output.record_token(token);
        self.thread_set_joiner(target_slot, token);
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

    fn take_current_join_result(
        &mut self,
    ) -> Option<(
        core::result::Result<usize, JoinError>,
        Option<UserThreadResources>,
    )> {
        let current = self
            .running_thread()
            .unwrap_or_else(|| panic!("thread: join result without running thread"));
        self.take_join_result(current.index())
            .map(|result| (result, self.take_reaped_user(current.index())))
    }

    fn running_user_address_space(&self) -> Option<AddressSpaceId> {
        let current = self.running_thread()?;
        self.thread_address_space(current.index())
    }

    fn replace_current_user_resources(
        &mut self,
        address_space: AddressSpaceId,
        stack: OwnedUserStack,
    ) -> core::result::Result<UserThreadResources, ((), OwnedUserStack)> {
        let Some(current) = self.running_thread() else {
            return Err(((), stack));
        };
        if self.thread_address_space(current.index()).is_none() {
            return Err(((), stack));
        }

        // SAFETY: execve commits a fully built TTBR0 root for the current user
        // process before replacing the trap frame that will resume to EL0.
        if unsafe { crate::memory::vm::activate_user_address_space(address_space) }.is_err() {
            return Err(((), stack));
        }
        self.replace_current_user_payload(UserThreadResources::new(address_space, stack))
            .map_err(|_| panic!("sched: current user resources disappeared during exec commit"))
    }
}

fn take_current_join_result_irqsave() -> (
    core::result::Result<usize, JoinError>,
    Option<UserThreadResources>,
) {
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    scheduler_mut()
        .on_cpu(cpu)
        .take_current_join_result()
        .unwrap_or((Err(JoinError::InvalidThread), None))
}

pub(super) extern "C" fn thread_entry_bootstrap(entry_addr: usize, arg: usize) -> ! {
    // SAFETY: frame setup passes a `ThreadEntry` function pointer in x0 and a
    // raw `ThreadArg` payload in x1.
    let entry_fn: ThreadEntry = unsafe { mem::transmute(entry_addr) };
    let code = entry_fn(ThreadArg::from_usize(arg));
    thread_exit(code)
}
