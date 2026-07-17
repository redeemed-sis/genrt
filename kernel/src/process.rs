use alloc::vec::Vec;
use core::{cell::UnsafeCell, fmt, mem};

use crate::{
    arch::ActiveContext,
    config::KERNEL_THREAD_CAPACITY,
    errno,
    fs::fd::{FdError, FdTable},
    fs::initramfs::{self, InitramfsError},
    fs::{path, ramfs},
    loader::elf::{self, ElfLoadError, UserElfImage, UserElfSegment},
    memory::{
        self,
        user::{self, OwnedUserStack, USER_STACK_TOP},
        vm::{self, OwnedUserAddressSpace, VmError},
    },
    sched::ThreadId,
    sched::{self, CommitResult, ThreadAttrs, WaitCause, WaitToken},
    sync::LocalIrqGuard,
};

pub(crate) const USER_STACK_SIZE: usize = 64 * 1024;

const MAX_PROCESSES: usize = 4;
const INITIAL_PROCESS_GENERATION: u32 = 1;
const PROCESS_ID_INDEX_BITS: usize = process_id_index_bits(MAX_PROCESSES);
const PROCESS_ID_INDEX_MASK: usize = (1 << PROCESS_ID_INDEX_BITS) - 1;

// AArch64 userspace starts with a compact SysV-like stack:
//   sp + 0:                 argc
//   sp + 8:                 argv[0]
//   ...
//                           argv[argc - 1]
//                           NULL
//                           envp[0]
//   ...
//                           NULL
//
// The argv/envp copy bound is therefore the fixed user stack itself: strings
// plus this pointer table must fit into `USER_STACK_SIZE`. There is no separate
// arbitrary argc/ARG_MAX limit in the process layer.
const EXEC_STACK_WORD_BYTES: usize = mem::size_of::<u64>();
const EXEC_STACK_ARGC_WORDS: usize = 1;
const EXEC_STACK_ARGV_NULL_WORDS: usize = 1;
const EXEC_STACK_ENVP_NULL_WORDS: usize = 1;

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct ProcessId {
    index: usize,
    generation: u32,
}

impl ProcessId {
    pub(crate) const fn new(index: usize, generation: u32) -> Self {
        Self { index, generation }
    }

    pub(crate) const fn index(self) -> usize {
        self.index
    }

    pub(crate) const fn generation(self) -> u32 {
        self.generation
    }

    pub(crate) const fn as_raw(self) -> usize {
        ((self.generation as usize) << PROCESS_ID_INDEX_BITS) | self.index
    }

    pub(crate) const fn from_raw(raw: usize) -> Option<Self> {
        if raw == 0 {
            return None;
        }
        let index = raw & PROCESS_ID_INDEX_MASK;
        let generation = (raw >> PROCESS_ID_INDEX_BITS) as u32;
        if index >= MAX_PROCESSES || generation == 0 {
            return None;
        }
        Some(Self { index, generation })
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.index, self.generation)
    }
}

impl fmt::Debug for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessId")
            .field("index", &self.index)
            .field("generation", &self.generation)
            .finish()
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProcessState {
    Free,
    Running,
    Zombie,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessExitStatus {
    Exited(usize),
    Faulted(UserFault),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UserFault {
    pub kind: UserFaultKind,
}

impl UserFault {
    pub const fn unknown_syscall(nr: usize) -> Self {
        Self {
            kind: UserFaultKind::UnknownSyscall(nr),
        }
    }

    pub const fn sync_exception(kind: UserFaultKind) -> Self {
        Self { kind }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
/// Stable classification of a fault attributed to a lower-EL process.
pub enum UserFaultKind {
    /// Userspace requested a syscall number not implemented by the kernel.
    UnknownSyscall(usize),
    /// Non-translation/non-permission instruction abort.
    InstructionAbort,
    /// Non-translation/non-permission data abort.
    DataAbort,
    /// Instruction fetch failed address translation.
    InstructionTranslationFault,
    /// Data access failed address translation.
    DataTranslationFault,
    /// Instruction fetch violated page permissions.
    InstructionPermissionFault,
    /// Data access violated page permissions.
    DataPermissionFault,
    /// Other lower-EL synchronous exception.
    OtherSync,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProcessError {
    InvalidProcess,
    NoProcessSlots,
    Vm(VmError),
    OutOfFrames,
    Spawn(sched::SpawnError),
    Elf(ElfLoadError),
    Initramfs(InitramfsError),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProcessJoinError {
    InvalidProcess,
    JoinInProgress,
    SelfJoin,
    SchedulerNotInitialized,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessFaultError {
    NoCurrentProcess,
    InvalidProcess,
}

/// Errors returned by process current-working-directory operations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProcessPathError {
    /// The caller is not running as a userspace process.
    NoCurrentProcess,
    /// The stored or requested ramfs directory index is invalid.
    InvalidDirectory,
}

struct ProcessSlot {
    generation: u32,
    state: ProcessState,
    address_space: Option<OwnedUserAddressSpace>,
    user_image: Option<UserElfImage>,
    fds: FdTable,
    cwd_dir: Option<usize>,
    parent: Option<ProcessId>,
    main_thread: Option<ThreadId>,
    exit_status: Option<ProcessExitStatus>,
    process_consumer: Option<ThreadId>,
    waiter: Option<WaitToken>,
}

impl ProcessSlot {
    const fn free() -> Self {
        Self {
            generation: 0,
            state: ProcessState::Free,
            address_space: None,
            user_image: None,
            fds: FdTable::new(),
            cwd_dir: None,
            parent: None,
            main_thread: None,
            exit_status: None,
            process_consumer: None,
            waiter: None,
        }
    }

    fn is_free(&self) -> bool {
        self.state == ProcessState::Free
    }
}

struct ProcessTable {
    slots: [ProcessSlot; MAX_PROCESSES],
    // O(1) current-process lookup keyed by `ThreadId::index`. An entry is
    // authoritative only when the referenced process slot still records the
    // exact generation-bearing `ThreadId` as its main thread.
    thread_owners: [Option<ProcessId>; KERNEL_THREAD_CAPACITY],
}

struct ReclaimedProcess {
    status: ProcessExitStatus,
    address_space: Option<OwnedUserAddressSpace>,
    user_image: Option<UserElfImage>,
}

struct ProcessResources {
    address_space: Option<OwnedUserAddressSpace>,
    user_image: Option<UserElfImage>,
}

struct StagedProcessImage {
    address_space: OwnedUserAddressSpace,
    user_image: UserElfImage,
    stack: OwnedUserStack,
    entry: usize,
    initial_sp: usize,
}

pub(crate) struct ExecArgs {
    argv: Vec<Vec<u8>>,
    envp: Vec<Vec<u8>>,
    string_bytes: usize,
}

#[derive(Copy, Clone)]
enum ExecStringVector {
    Argv,
    Envp,
}

impl ExecArgs {
    fn empty() -> Self {
        Self {
            argv: Vec::new(),
            envp: Vec::new(),
            string_bytes: 0,
        }
    }

    fn push(&mut self, vector: ExecStringVector, value: Vec<u8>) -> Result<(), errno::Errno> {
        let bytes_with_nul = value.len().checked_add(1).ok_or(errno::E2BIG)?;
        let next_argv =
            self.argv.len() + exec_vector_slot_increment(vector, ExecStringVector::Argv);
        let next_envp =
            self.envp.len() + exec_vector_slot_increment(vector, ExecStringVector::Envp);
        let budget = exec_arg_string_budget(next_argv, next_envp).ok_or(errno::E2BIG)?;
        let next_bytes = self
            .string_bytes
            .checked_add(bytes_with_nul)
            .ok_or(errno::E2BIG)?;
        if next_bytes > budget {
            return Err(errno::E2BIG);
        }

        match vector {
            ExecStringVector::Argv => {
                self.argv.try_reserve_exact(1).map_err(|_| errno::ENOMEM)?;
                self.argv.push(value);
            }
            ExecStringVector::Envp => {
                self.envp.try_reserve_exact(1).map_err(|_| errno::ENOMEM)?;
                self.envp.push(value);
            }
        }
        self.string_bytes = next_bytes;
        Ok(())
    }

    fn remaining_for_next(&self, vector: ExecStringVector) -> Result<usize, errno::Errno> {
        let next_argv =
            self.argv.len() + exec_vector_slot_increment(vector, ExecStringVector::Argv);
        let next_envp =
            self.envp.len() + exec_vector_slot_increment(vector, ExecStringVector::Envp);
        let budget = exec_arg_string_budget(next_argv, next_envp).ok_or(errno::E2BIG)?;
        budget.checked_sub(self.string_bytes).ok_or(errno::E2BIG)
    }
}

struct WaitedProcess {
    pid: ProcessId,
    status: ProcessExitStatus,
    address_space: Option<OwnedUserAddressSpace>,
    user_image: Option<UserElfImage>,
    main_thread: ThreadId,
    wait_token: Option<WaitToken>,
}

pub(crate) enum WaitPidAction {
    Return(Result<usize, errno::Errno>),
    Blocked,
}

enum ConsumeProcessError {
    WouldBlock,
    Join(ProcessJoinError),
}

impl ProcessTable {
    const fn new() -> Self {
        Self {
            slots: [const { ProcessSlot::free() }; MAX_PROCESSES],
            thread_owners: [None; KERNEL_THREAD_CAPACITY],
        }
    }
}

struct ProcessTableCell(UnsafeCell<ProcessTable>);

// SAFETY: genrt is single-core for this milestone. Process table mutations run
// either in short IRQ-disabled thread-context sections or in the synchronous
// lower-EL trap path for the currently running thread.
unsafe impl Sync for ProcessTableCell {}

static PROCESS_TABLE: ProcessTableCell = ProcessTableCell(UnsafeCell::new(ProcessTable::new()));

/// Find the process that owns the currently scheduled main thread.
///
/// A process-owned reverse index maps the current thread slot directly to a
/// generation-checked process. Scheduler state therefore remains free of
/// process metadata while lookup stays O(1). The operation runs with local IRQs
/// disabled, allocates nothing, and is never used from an interrupt handler.
fn current_process_id() -> Option<ProcessId> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let thread = sched::current_thread_id()?;
    table_mut().process_for_thread(thread)
}

pub(crate) fn spawn_first_user_process() -> Result<ProcessId, ProcessError> {
    let cwd_dir = ramfs::root_dir_index().ok_or(ProcessError::InvalidProcess)?;
    let pid = allocate_process_slot(None, FdTable::new(), cwd_dir)?;
    let mut address_space = Some(vm::create_user_address_space().map_err(|err| {
        free_process_slot(pid);
        ProcessError::Vm(err)
    })?);
    let mut stack = None;
    let mut user_image = None;

    let result = (|| {
        let aspace = address_space.as_mut().ok_or(ProcessError::InvalidProcess)?;
        let loaded = load_init_image(aspace)?;
        let entry = loaded.entry;
        user_image = Some(loaded);

        stack = Some(OwnedUserStack::allocate(aspace, USER_STACK_SIZE).map_err(ProcessError::Vm)?);
        let mut exec_args = ExecArgs::empty();
        exec_args
            .push(ExecStringVector::Argv, b"/init".to_vec())
            .map_err(|_| ProcessError::OutOfFrames)?;
        let initial_sp = build_initial_user_stack(
            stack.as_ref().ok_or(ProcessError::InvalidProcess)?,
            &exec_args,
        )
        .ok_or(ProcessError::OutOfFrames)?;
        attach_process_resources(
            pid,
            address_space.take().ok_or(ProcessError::InvalidProcess)?,
            user_image.take().ok_or(ProcessError::InvalidProcess)?,
        );

        {
            let _irq_guard = LocalIrqGuard::save_and_disable();
            let main_thread = sched::thread_spawn_user(
                table_mut()
                    .slot_mut(pid)
                    .ok_or(ProcessError::InvalidProcess)?
                    .address_space
                    .as_ref()
                    .ok_or(ProcessError::InvalidProcess)?
                    .id(),
                stack.take().ok_or(ProcessError::InvalidProcess)?,
                entry,
                initial_sp,
                0,
                ThreadAttrs::joinable(),
            )
            .map_err(|(error, returned_stack)| {
                stack = Some(returned_stack);
                ProcessError::Spawn(error)
            })?;
            attach_process_main_thread(pid, main_thread);
        }
        Ok(pid)
    })();

    if result.is_err() {
        let attached = take_process_resources(pid);
        if let Some(image) = user_image {
            elf::free_loaded_segments(&image);
        } else if let Some(image) = attached.user_image {
            elf::free_loaded_segments(&image);
        }
        drop(stack);
        let address_space = attached
            .address_space
            .or(address_space)
            .ok_or(ProcessError::InvalidProcess)?;
        // SAFETY: a failed spawn path never leaves a runnable user thread that
        // can still reference this address space.
        let _ = unsafe { vm::destroy_user_address_space(address_space) };
        free_process_slot(pid);
    }

    result
}

pub(crate) fn process_join(pid: ProcessId) -> Result<ProcessExitStatus, ProcessJoinError> {
    if current_process_id() == Some(pid) {
        return Err(ProcessJoinError::SelfJoin);
    }

    let caller = sched::current_thread_id().ok_or(ProcessJoinError::SchedulerNotInitialized)?;
    let main_thread = claim_process_consumer(pid, caller)?;
    sched::thread_join(main_thread).map_err(|error| match error {
        sched::JoinError::JoinInProgress => ProcessJoinError::JoinInProgress,
        sched::JoinError::SchedulerNotInitialized => ProcessJoinError::SchedulerNotInitialized,
        _ => ProcessJoinError::InvalidProcess,
    })?;

    let reclaimed = match consume_process_for_join(pid, caller) {
        Ok(reclaimed) => reclaimed,
        Err(ConsumeProcessError::WouldBlock) => {
            return Err(ProcessJoinError::InvalidProcess);
        }
        Err(ConsumeProcessError::Join(err)) => return Err(err),
    };
    cleanup_reclaimed_process(pid, reclaimed)
}

fn claim_process_consumer(pid: ProcessId, caller: ThreadId) -> Result<ThreadId, ProcessJoinError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let slot = table_mut()
        .slot_mut(pid)
        .ok_or(ProcessJoinError::InvalidProcess)?;
    if let Some(consumer) = slot.process_consumer {
        return if consumer == caller {
            Err(ProcessJoinError::JoinInProgress)
        } else {
            Err(ProcessJoinError::JoinInProgress)
        };
    }
    let main_thread = slot.main_thread.ok_or(ProcessJoinError::InvalidProcess)?;
    slot.process_consumer = Some(caller);
    Ok(main_thread)
}

/// Store normal process exit status and switch away from its current thread.
///
/// # Arguments
///
/// * `context` - Exclusive live userspace context replaced by scheduler exit.
/// * `code` - Userspace exit code stored in process and thread state.
///
/// # Returns
///
/// Returns only in the exception handler after the live frame has been replaced
/// with another runnable thread; the exiting userspace thread does not resume.
/// The terminal transition is bounded and does not allocate.
///
/// # Panics
///
/// Panics if no current process can own the exit or a
/// [`crate::sync::preempt::PreemptGuard`] is active before terminal state changes.
pub(crate) fn process_exit_current(context: &mut ActiveContext<'_>, code: usize) {
    // Safe point: a terminal process handoff cannot publish exit state while a
    // thread-only preemption guard still owns its protected access.
    crate::sync::preempt::assert_preemption_enabled("process exit state change");
    finish_current_process(context, ProcessExitStatus::Exited(code), code)
        .unwrap_or_else(|err| panic!("process: sys_exit without current process: {err:?}"));
}

/// Attribute a lower-EL fault to the current process and switch it out.
///
/// # Arguments
///
/// * `context` - Exclusive live lower-EL fault context replaced by scheduler
///   exit.
/// * `fault` - Architecture-classified userspace fault record.
///
/// # Returns
///
/// Returns `Ok(())` after storing fault status, waking registered waiters, and
/// replacing the live context. The path is bounded and does not allocate.
///
/// # Errors
///
/// Returns [`ProcessFaultError::NoCurrentProcess`] when no user process is
/// running or [`ProcessFaultError::InvalidProcess`] for stale process state.
///
/// # Panics
///
/// Panics when a `crate::sync::preempt::PreemptGuard` is active before terminal state
/// changes.
pub fn kill_current_process_on_user_fault(
    context: &mut ActiveContext<'_>,
    fault: UserFault,
) -> Result<(), ProcessFaultError> {
    crate::sync::preempt::assert_preemption_enabled("process fault exit state change");
    finish_current_process(context, ProcessExitStatus::Faulted(fault), usize::MAX)
}

/// Eagerly clone the current process and live userspace resume context.
///
/// This thread-context operation may allocate and copy process-owned memory. Only
/// the final child publication and scheduler-frame clone run in a short local
/// IRQ-disabled section; scheduler storage remains preallocated.
///
/// # Arguments
///
/// * `context` - Exclusive live parent syscall context cloned into the child
///   before the parent return value is written.
///
/// # Returns
///
/// Returns the generation-encoded child PID to the parent. The architecture
/// saved-frame clone hook preserves existing fork child return semantics.
///
/// # Errors
///
/// Returns a POSIX errno for invalid process state, exhausted process/thread or
/// frame capacity, VM failure, or eager-copy failure.
///
/// # Panics
///
/// Panics if a reserved child process slot disappears after transactional
/// publication begins; that would violate single-core process-table ownership.
pub(crate) fn fork_current(context: &mut ActiveContext<'_>) -> Result<usize, errno::Errno> {
    let snapshot = fork_snapshot_current()?;
    let child_pid =
        allocate_process_slot(Some(snapshot.parent_pid), snapshot.fds, snapshot.cwd_dir)
            .map_err(fork_errno)?;
    let mut address_space = None;
    let mut user_image = None;
    let mut stack = None;

    let result = (|| {
        // Publish ownership to the rollback state before any clone can fail:
        // OwnedUserAddressSpace has no Drop and must be explicitly destroyed.
        address_space = Some(vm::create_user_address_space().map_err(fork_vm_errno)?);
        let child_aspace = address_space.as_ref().ok_or(errno::ENOMEM)?;

        let child_image = clone_user_image(snapshot.user_image(), child_aspace)?;
        user_image = Some(child_image);

        // SAFETY: the pointer names the currently running parent thread's
        // stack. This thread cannot be reaped while it synchronously clones its
        // own fork image.
        stack =
            Some(unsafe { (&*snapshot.stack).clone_into(child_aspace) }.map_err(fork_vm_errno)?);
        let child_aspace_id = child_aspace.id();
        attach_process_resources(
            child_pid,
            address_space.take().ok_or(errno::ENOMEM)?,
            user_image.take().ok_or(errno::ENOMEM)?,
        );

        {
            let _irq_guard = LocalIrqGuard::save_and_disable();
            let child_thread = sched::thread_spawn_user_from_context(
                child_aspace_id,
                stack.take().ok_or(errno::ENOMEM)?,
                context,
                ThreadAttrs::joinable(),
            )
            .map_err(|(error, returned_stack)| {
                stack = Some(returned_stack);
                spawn_errno(error)
            })?;
            attach_process_main_thread(child_pid, child_thread);
        }

        Ok(child_pid.as_raw())
    })();

    if result.is_err() {
        let attached = take_process_resources(child_pid);
        if let Some(image) = user_image {
            elf::free_loaded_segments(&image);
        } else if let Some(image) = attached.user_image {
            elf::free_loaded_segments(&image);
        }
        drop(stack);
        let attached_aspace = attached.address_space;
        if let Some(aspace) = attached_aspace.or(address_space) {
            // SAFETY: failed fork did not publish a runnable child using this
            // address space, or the child slot is reclaimed immediately below.
            let _ = unsafe { vm::destroy_user_address_space(aspace) };
        }
        free_process_slot(child_pid);
    }

    result
}

/// Replace the current process image while preserving process-owned state.
///
/// File descriptors, PID relationships, and cwd remain unchanged. Only the
/// address space, loaded ELF segments, user stack, and active user context are
/// replaced.
///
/// # Arguments
///
/// * `context` - Exclusive live userspace syscall context to replace after
///   committing the new image.
/// * `path` - Canonical resolved executable path and directory requirement.
/// * `args` - Kernel-owned argv/envp strings for the new image.
///
/// # Returns
///
/// Returns `Ok(())` after committing the new image.
///
/// # Errors
///
/// Returns a POSIX errno for invalid process state, missing/invalid executable,
/// allocation failure, ELF rejection, or VM setup failure.
///
/// This thread-context operation may allocate, parse ELF data, and copy process
/// memory. Those operations occur outside scheduler and IRQ fast paths.
pub(crate) fn execve_current(
    context: &mut ActiveContext<'_>,
    path: path::ResolvedPath,
    args: ExecArgs,
) -> Result<(), errno::Errno> {
    let pid = current_process_id().ok_or(errno::EINVAL)?;
    let staged = stage_exec_image(&path, &args)?;
    let (old, old_stack) = commit_exec_image(pid, staged, context);
    drop(old_stack);
    cleanup_process_resources(pid, old);
    Ok(())
}

pub(crate) fn copy_exec_args_from_user(
    path: &[u8],
    argv_ptr: usize,
    envp_ptr: usize,
) -> Result<ExecArgs, errno::Errno> {
    let mut args = ExecArgs::empty();

    if argv_ptr == 0 {
        args.push(ExecStringVector::Argv, path.to_vec())?;
    } else {
        copy_exec_string_vector_from_user(argv_ptr, ExecStringVector::Argv, &mut args)?;
    }

    if envp_ptr != 0 {
        copy_exec_string_vector_from_user(envp_ptr, ExecStringVector::Envp, &mut args)?;
    }

    Ok(args)
}

/// Consume or block for one specific child process status.
///
/// The prepare/register/block transition runs in a short IRQ-disabled section
/// and does not allocate. Resource cleanup and userspace status copying occur
/// after that section.
///
/// # Arguments
///
/// * `context` - Exclusive live syscall context restarted and handed to the
///   scheduler when the child is still running.
/// * `raw_pid` - Positive generation-encoded child PID from userspace.
/// * `status_ptr` - Optional userspace pointer receiving encoded wait status.
///
/// # Returns
///
/// Returns [`WaitPidAction::Return`] with a PID or errno when complete, or
/// [`WaitPidAction::Blocked`] after registering a restartable wait.
pub(crate) fn waitpid_current(
    context: &mut ActiveContext<'_>,
    raw_pid: isize,
    status_ptr: usize,
) -> WaitPidAction {
    // This bring-up ABI supports the shell's explicit child wait:
    // `waitpid(child_pid, status, 0)`. `waitpid(-1)` needs a parent-level
    // "any child" wait list so a different child cannot exit between child
    // selection and blocking.
    if raw_pid <= 0 {
        return WaitPidAction::Return(Err(errno::ECHILD));
    }
    let Some(child_pid) = ProcessId::from_raw(raw_pid as usize) else {
        return WaitPidAction::Return(Err(errno::ECHILD));
    };

    if status_ptr != 0
        && user::validate_user_write_range(status_ptr, mem::size_of::<u32>())
            .map_err(wait_user_copy_errno)
            .is_err()
    {
        return WaitPidAction::Return(Err(errno::EFAULT));
    }

    let parent_pid = match current_process_id() {
        Some(pid) => pid,
        None => return WaitPidAction::Return(Err(errno::EINVAL)),
    };
    let caller = match sched::current_thread_id() {
        Some(thread) => thread,
        None => return WaitPidAction::Return(Err(errno::EINVAL)),
    };

    let prepare = {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        prepare_wait_child_locked(parent_pid, child_pid, caller)
    };

    match prepare {
        WaitChildPrepare::Consumed(waited) => {
            if let Some(token) = waited.wait_token {
                match sched::finish_wait(token) {
                    Ok(_) | Err(sched::FinishError::Stale) => {}
                    Err(sched::FinishError::NotCompleted) => {
                        panic!("waitpid: child wait not completed")
                    }
                }
            }
            let encoded = encode_wait_status(waited.status);
            if sched::thread_join(waited.main_thread).is_err() {
                return WaitPidAction::Return(Err(errno::ECHILD));
            }
            cleanup_waited_process(waited);
            if status_ptr != 0 {
                let bytes = encoded.to_le_bytes();
                if user::copy_to_user(status_ptr, &bytes)
                    .map_err(wait_user_copy_errno)
                    .is_err()
                {
                    return WaitPidAction::Return(Err(errno::EFAULT));
                }
            }
            WaitPidAction::Return(Ok(child_pid.as_raw()))
        }
        WaitChildPrepare::Prepared(prepared) => {
            context.restart_current_syscall();
            match sched::commit_wait(context, prepared) {
                CommitResult::Blocked(_) | CommitResult::Early(_) => WaitPidAction::Blocked,
                CommitResult::Stale => panic!("waitpid: prepared wait became stale before commit"),
            }
        }
        WaitChildPrepare::Err(errno) => WaitPidAction::Return(Err(errno)),
    }
}

fn finish_current_process(
    context: &mut ActiveContext<'_>,
    status: ProcessExitStatus,
    thread_code: usize,
) -> Result<(), ProcessFaultError> {
    crate::sync::preempt::assert_preemption_enabled("process terminal state change");
    let pid = current_process_id().ok_or(ProcessFaultError::NoCurrentProcess)?;
    let thread = sched::current_thread_id();
    let wake = {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        finish_process(pid, status).ok_or(ProcessFaultError::InvalidProcess)?
    };

    match status {
        ProcessExitStatus::Exited(code) => {
            crate::debug!("process: pid={pid} exited code={code} thread={thread:?}");
        }
        ProcessExitStatus::Faulted(fault) => {
            crate::warn!(
                "process: pid={pid} faulted kind={:?} thread={thread:?}",
                fault.kind,
            );
        }
    }

    if let Some(waiter) = wake {
        let _ = sched::complete_wait(waiter, WaitCause::Notified);
    }
    sched::on_thread_exit_sync(context, thread_code);
    Ok(())
}

fn allocate_process_slot(
    parent: Option<ProcessId>,
    fds: FdTable,
    cwd_dir: usize,
) -> Result<ProcessId, ProcessError> {
    if ramfs::dir_path(cwd_dir).is_none() {
        return Err(ProcessError::InvalidProcess);
    }
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let Some(index) = table
        .slots
        .iter()
        .enumerate()
        .find_map(|(index, slot)| slot.is_free().then_some(index))
    else {
        return Err(ProcessError::NoProcessSlots);
    };

    let generation = next_generation(table.slots[index].generation);
    table.slots[index] = ProcessSlot {
        generation,
        state: ProcessState::Running,
        address_space: None,
        user_image: None,
        fds,
        cwd_dir: Some(cwd_dir),
        parent,
        main_thread: None,
        exit_status: None,
        process_consumer: None,
        waiter: None,
    };

    let pid = ProcessId::new(index, generation);
    crate::debug!("process: allocated slot pid={pid}");
    Ok(pid)
}

fn attach_process_resources(
    pid: ProcessId,
    address_space: OwnedUserAddressSpace,
    user_image: UserElfImage,
) {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table
        .slot_mut(pid)
        .unwrap_or_else(|| panic!("process: reserved slot disappeared before resource attach"));
    slot.address_space = Some(address_space);
    slot.user_image = Some(user_image);
    slot.state = ProcessState::Running;
    crate::debug!("process: pid={pid} resources ready",);
}

fn attach_process_main_thread(pid: ProcessId, main_thread: ThreadId) {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    table.bind_main_thread(pid, main_thread);
    crate::debug!("process: pid={pid} running main={main_thread}");
}

fn finish_process(pid: ProcessId, status: ProcessExitStatus) -> Option<Option<WaitToken>> {
    let table = table_mut();
    let waiter = {
        let slot = table.slot_mut(pid)?;
        if slot.exit_status.is_some() {
            return Some(None);
        }

        slot.fds.close_all();
        slot.cwd_dir = None;
        slot.state = ProcessState::Zombie;
        slot.exit_status = Some(status);
        slot.waiter
    };
    table.unbind_main_thread(pid);
    Some(waiter)
}

struct ForkSnapshot {
    parent_pid: ProcessId,
    user_image: *const UserElfImage,
    stack: *const OwnedUserStack,
    fds: FdTable,
    cwd_dir: usize,
}

impl ForkSnapshot {
    fn user_image(&self) -> &UserElfImage {
        // SAFETY: the pointer targets the current process slot. genrt currently
        // has one user thread per process, so the current process cannot mutate
        // or reclaim its own image while fork clones resources in thread context.
        unsafe { &*self.user_image }
    }
}

enum WaitChildPrepare {
    Consumed(WaitedProcess),
    Prepared(sched::PreparedWait),
    Err(errno::Errno),
}

fn fork_snapshot_current() -> Result<ForkSnapshot, errno::Errno> {
    let parent_pid = current_process_id().ok_or(errno::EINVAL)?;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table.slot_mut(parent_pid).ok_or(errno::EINVAL)?;
    let user_image = slot.user_image.as_ref().ok_or(errno::EINVAL)? as *const UserElfImage;
    let cwd_dir = slot.cwd_dir.ok_or(errno::EINVAL)?;

    Ok(ForkSnapshot {
        parent_pid,
        user_image,
        // SAFETY: synchronous fork cannot reap its currently running parent
        // thread before the clone consumes this pointer.
        stack: unsafe { sched::current_user_stack_ptr() }.ok_or(errno::EINVAL)?,
        fds: slot.fds,
        cwd_dir,
    })
}

fn clone_user_image(
    src: &UserElfImage,
    dst_aspace: &OwnedUserAddressSpace,
) -> Result<UserElfImage, errno::Errno> {
    let mut segments = Vec::new();
    segments
        .try_reserve_exact(src.segments().len())
        .map_err(|_| errno::ENOMEM)?;

    for segment in src.segments() {
        let cloned = clone_user_segment(*segment, dst_aspace);
        match cloned {
            Ok(segment) => segments.push(segment),
            Err(errno) => {
                free_user_segments(&segments);
                return Err(errno);
            }
        }
    }

    Ok(UserElfImage::from_segments(src.entry, segments))
}

fn clone_user_segment(
    segment: UserElfSegment,
    dst_aspace: &OwnedUserAddressSpace,
) -> Result<UserElfSegment, errno::Errno> {
    let frames = memory::clone_frame_range(segment.frames).map_err(clone_frame_errno)?;
    if let Err(err) = vm::map_user_page_range(
        dst_aspace,
        segment.va,
        frames.start,
        segment.size,
        segment.flags,
    ) {
        memory::free_contiguous_frames(frames);
        return Err(fork_vm_errno(err));
    }

    Ok(UserElfSegment { frames, ..segment })
}

fn stage_exec_image(
    path: &path::ResolvedPath,
    args: &ExecArgs,
) -> Result<StagedProcessImage, errno::Errno> {
    let file_index = match path.node {
        path::ResolvedNode::File { file_index } => file_index,
        path::ResolvedNode::Directory { .. } => return Err(errno::EACCES),
    };
    let image = ramfs::data(file_index).ok_or(errno::ENOENT)?;

    let mut address_space = Some(vm::create_user_address_space().map_err(exec_vm_errno)?);
    let mut stack = None;
    let mut user_image = None;

    let result = (|| {
        let aspace = address_space.as_mut().ok_or(errno::ENOMEM)?;
        let loaded = elf::load_user_elf(image, aspace).map_err(exec_elf_errno)?;
        let entry = loaded.entry;
        user_image = Some(loaded);

        stack = Some(OwnedUserStack::allocate(aspace, USER_STACK_SIZE).map_err(exec_vm_errno)?);
        let initial_sp = build_initial_user_stack(stack.as_ref().ok_or(errno::ENOMEM)?, args)
            .ok_or(errno::E2BIG)?;

        Ok(StagedProcessImage {
            address_space: address_space.take().ok_or(errno::ENOMEM)?,
            user_image: user_image.take().ok_or(errno::ENOMEM)?,
            stack: stack.take().ok_or(errno::ENOMEM)?,
            entry,
            initial_sp,
        })
    })();

    if result.is_err() {
        if let Some(image) = user_image {
            elf::free_loaded_segments(&image);
        }
        drop(stack);
        // SAFETY: staged exec has not been published to the scheduler/process
        // table yet, so no running thread can reference this root.
        if let Some(address_space) = address_space {
            let _ = unsafe { vm::destroy_user_address_space(address_space) };
        }
    }

    result
}

fn commit_exec_image(
    pid: ProcessId,
    staged: StagedProcessImage,
    context: &mut ActiveContext<'_>,
) -> (ProcessResources, crate::sched::UserThreadResources) {
    let entry = staged.entry;
    let initial_sp = staged.initial_sp;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    if table_mut().slot_mut(pid).is_none() {
        panic!("process: validated exec slot disappeared before commit");
    }
    let old_stack = sched::replace_current_user_resources(staged.address_space.id(), staged.stack)
        .unwrap_or_else(|_| panic!("process: staged exec address-space activation failed"));
    let old = replace_process_resources(pid, staged.address_space, staged.user_image);

    // The current thread now points at the new TTBR0 root and the stack was
    // populated through HVA before commit. The architecture facade preserves
    // the EL1 kernel stack while replacing all EL0-visible state.
    context.replace_user_context_after_exec(entry, initial_sp);

    (old, old_stack)
}

fn replace_process_resources(
    pid: ProcessId,
    address_space: OwnedUserAddressSpace,
    user_image: UserElfImage,
) -> ProcessResources {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table
        .slot_mut(pid)
        .unwrap_or_else(|| panic!("process: prevalidated exec slot disappeared during swap"));
    let old = ProcessResources {
        address_space: slot.address_space.replace(address_space),
        user_image: slot.user_image.replace(user_image),
    };
    slot.state = ProcessState::Running;
    slot.exit_status = None;
    old
}

fn cleanup_process_resources(pid: ProcessId, resources: ProcessResources) {
    if let Some(image) = resources.user_image {
        elf::free_loaded_segments(&image);
    }
    if let Some(address_space) = resources.address_space {
        // SAFETY: resources were atomically removed from the process slot and
        // scheduler before cleanup, so no runnable thread references this root.
        if let Err(err) = unsafe { vm::destroy_user_address_space(address_space) } {
            crate::warn!("process: failed to destroy old address space pid={pid}: {err:?}");
        }
    }
}

fn build_initial_user_stack(stack: &OwnedUserStack, args: &ExecArgs) -> Option<usize> {
    let stack_base = stack.base();
    let argc = args.argv.len();
    let envc = args.envp.len();
    let mut sp = USER_STACK_TOP;
    let mut arg_ptrs = Vec::new();
    let mut env_ptrs = Vec::new();
    arg_ptrs.try_reserve_exact(argc).ok()?;
    env_ptrs.try_reserve_exact(envc).ok()?;

    for env in args.envp.iter().rev() {
        let ptr = push_stack_cstr(stack, stack_base, &mut sp, env)?;
        env_ptrs.push(ptr as u64);
    }
    env_ptrs.reverse();

    for arg in args.argv.iter().rev() {
        let ptr = push_stack_cstr(stack, stack_base, &mut sp, arg)?;
        arg_ptrs.push(ptr as u64);
    }
    arg_ptrs.reverse();

    sp &= !0xf;
    let table_words = exec_stack_table_words(argc, envc)?;
    let table_size = table_words.checked_mul(EXEC_STACK_WORD_BYTES)?;
    sp = sp.checked_sub(table_size)? & !0xf;
    if sp < stack_base {
        return None;
    }

    write_stack_u64(stack, stack_base, sp, argc as u64)?;
    let mut word = 1usize;
    for ptr in arg_ptrs {
        write_stack_u64(stack, stack_base, stack_word_addr(sp, word)?, ptr)?;
        word += 1;
    }
    write_stack_u64(stack, stack_base, stack_word_addr(sp, word)?, 0)?;
    word += 1;
    for ptr in env_ptrs {
        write_stack_u64(stack, stack_base, stack_word_addr(sp, word)?, ptr)?;
        word += 1;
    }
    write_stack_u64(stack, stack_base, stack_word_addr(sp, word)?, 0)?;

    Some(sp)
}

fn push_stack_cstr(
    stack: &OwnedUserStack,
    stack_base: usize,
    sp: &mut usize,
    bytes: &[u8],
) -> Option<usize> {
    *sp = (*sp).checked_sub(bytes.len().checked_add(1)?)?;
    if *sp < stack_base {
        return None;
    }
    write_stack_bytes(stack, stack_base, *sp, bytes)?;
    write_stack_byte(stack, stack_base, *sp + bytes.len(), 0)?;
    Some(*sp)
}

fn write_stack_bytes(
    stack: &OwnedUserStack,
    stack_base: usize,
    user_va: usize,
    bytes: &[u8],
) -> Option<()> {
    if bytes.is_empty() {
        return Some(());
    }
    let offset = user_va.checked_sub(stack_base)?;
    if offset.checked_add(bytes.len())? > USER_STACK_SIZE {
        return None;
    }
    memory::copy_bytes_to_phys(stack.frames().start + offset, bytes);
    Some(())
}

fn write_stack_byte(
    stack: &OwnedUserStack,
    stack_base: usize,
    user_va: usize,
    byte: u8,
) -> Option<()> {
    let offset = user_va.checked_sub(stack_base)?;
    if offset >= USER_STACK_SIZE {
        return None;
    }
    memory::copy_bytes_to_phys(stack.frames().start + offset, &[byte]);
    Some(())
}

fn write_stack_u64(
    stack: &OwnedUserStack,
    stack_base: usize,
    user_va: usize,
    value: u64,
) -> Option<()> {
    write_stack_bytes(stack, stack_base, user_va, &value.to_le_bytes())
}

fn prepare_wait_child_locked(
    parent_pid: ProcessId,
    child_pid: ProcessId,
    caller: ThreadId,
) -> WaitChildPrepare {
    let table = table_mut();
    let Some(slot) = table.slot_mut(child_pid) else {
        return WaitChildPrepare::Err(errno::ECHILD);
    };
    if slot.parent != Some(parent_pid) {
        return WaitChildPrepare::Err(errno::ECHILD);
    }
    match slot.process_consumer {
        Some(consumer) if consumer != caller => return WaitChildPrepare::Err(errno::ECHILD),
        Some(_) => {}
        None => slot.process_consumer = Some(caller),
    }
    if let Some(waiter) = slot.waiter {
        if waiter.thread() != caller {
            return WaitChildPrepare::Err(errno::ECHILD);
        }
    }

    let Some(status) = slot.exit_status.take() else {
        if slot.waiter.is_some() {
            panic!("waitpid: current caller already owns an unfinished child wait");
        }
        crate::sync::preempt::assert_preemption_enabled("process waiter publication");
        let prepared = sched::prepare_wait();
        slot.waiter = Some(prepared.token());
        return WaitChildPrepare::Prepared(prepared);
    };

    let main_thread = slot
        .main_thread
        .unwrap_or_else(|| panic!("waitpid: terminal process has no main thread"));
    let waited = WaitedProcess {
        pid: child_pid,
        status,
        address_space: slot.address_space.take(),
        user_image: slot.user_image.take(),
        main_thread,
        wait_token: slot.waiter.take(),
    };
    let next_generation = next_generation(slot.generation);
    table.release_slot(child_pid, next_generation);
    WaitChildPrepare::Consumed(waited)
}

fn cleanup_waited_process(waited: WaitedProcess) {
    cleanup_process_resources(
        waited.pid,
        ProcessResources {
            address_space: waited.address_space,
            user_image: waited.user_image,
        },
    );
    crate::debug!(
        "waitpid: reclaimed child pid={} status={:?}",
        waited.pid,
        waited.status
    );
}

fn encode_wait_status(status: ProcessExitStatus) -> u32 {
    match status {
        ProcessExitStatus::Exited(code) => ((code as u32) & 0xff) << 8,
        ProcessExitStatus::Faulted(_) => 0x7f,
    }
}

const fn process_id_index_bits(slots: usize) -> usize {
    let mut bits = 0usize;
    let mut capacity = 1usize;
    while capacity < slots {
        bits += 1;
        capacity <<= 1;
    }
    bits
}

fn exec_stack_table_words(argc: usize, envc: usize) -> Option<usize> {
    EXEC_STACK_ARGC_WORDS
        .checked_add(argc)?
        .checked_add(EXEC_STACK_ARGV_NULL_WORDS)?
        .checked_add(envc)?
        .checked_add(EXEC_STACK_ENVP_NULL_WORDS)
}

fn exec_arg_string_budget(argc: usize, envc: usize) -> Option<usize> {
    let table_bytes = exec_stack_table_words(argc, envc)?.checked_mul(EXEC_STACK_WORD_BYTES)?;
    USER_STACK_SIZE.checked_sub(table_bytes)
}

fn exec_vector_slot_increment(vector: ExecStringVector, target: ExecStringVector) -> usize {
    if matches!(
        (vector, target),
        (ExecStringVector::Argv, ExecStringVector::Argv)
            | (ExecStringVector::Envp, ExecStringVector::Envp)
    ) {
        1
    } else {
        0
    }
}

fn stack_word_addr(sp: usize, word: usize) -> Option<usize> {
    sp.checked_add(word.checked_mul(EXEC_STACK_WORD_BYTES)?)
}

fn read_user_usize(ptr: usize) -> Result<usize, errno::Errno> {
    let mut bytes = [0u8; mem::size_of::<usize>()];
    user::copy_from_user(&mut bytes, ptr).map_err(exec_user_copy_errno)?;
    Ok(usize::from_le_bytes(bytes))
}

fn copy_exec_string_vector_from_user(
    vector_ptr: usize,
    vector: ExecStringVector,
    args: &mut ExecArgs,
) -> Result<(), errno::Errno> {
    let mut index = 0usize;
    loop {
        let ptr = read_user_usize(
            vector_ptr
                .checked_add(
                    index
                        .checked_mul(mem::size_of::<usize>())
                        .ok_or(errno::EFAULT)?,
                )
                .ok_or(errno::EFAULT)?,
        )?;
        if ptr == 0 {
            return Ok(());
        }

        let remaining = args.remaining_for_next(vector)?;
        let max_string_len = remaining.checked_sub(1).ok_or(errno::E2BIG)?;
        let value = user::copy_cstr_from_user(ptr, max_string_len).map_err(exec_arg_copy_errno)?;
        args.push(vector, value)?;
        index = index.checked_add(1).ok_or(errno::E2BIG)?;
    }
}

fn free_user_segments(segments: &[UserElfSegment]) {
    for segment in segments {
        if segment.frames.start != 0 {
            memory::free_contiguous_frames(segment.frames);
        }
    }
}

fn fork_errno(err: ProcessError) -> errno::Errno {
    match err {
        ProcessError::NoProcessSlots => errno::EAGAIN,
        ProcessError::OutOfFrames => errno::ENOMEM,
        ProcessError::Vm(err) => fork_vm_errno(err),
        ProcessError::Spawn(_) => errno::EAGAIN,
        ProcessError::InvalidProcess => errno::EINVAL,
        ProcessError::Elf(_) | ProcessError::Initramfs(_) => errno::ENOEXEC,
    }
}

fn fork_vm_errno(err: VmError) -> errno::Errno {
    match err {
        VmError::OutOfFrames => errno::ENOMEM,
        _ => errno::EINVAL,
    }
}

fn clone_frame_errno(err: memory::FrameRangeCloneError) -> errno::Errno {
    match err {
        memory::FrameRangeCloneError::InvalidRange => errno::EINVAL,
        memory::FrameRangeCloneError::OutOfFrames => errno::ENOMEM,
    }
}

fn spawn_errno(err: sched::SpawnError) -> errno::Errno {
    match err {
        sched::SpawnError::NoThreadSlots | sched::SpawnError::NoStackSlots => errno::EAGAIN,
        sched::SpawnError::SchedulerNotInitialized
        | sched::SpawnError::UserThreadMustBeJoinable => errno::EINVAL,
    }
}

fn exec_vm_errno(err: VmError) -> errno::Errno {
    match err {
        VmError::OutOfFrames => errno::ENOMEM,
        _ => errno::EINVAL,
    }
}

fn exec_elf_errno(err: ElfLoadError) -> errno::Errno {
    match err {
        ElfLoadError::FrameAllocationFailed => errno::ENOMEM,
        _ => errno::ENOEXEC,
    }
}

fn exec_user_copy_errno(err: user::UserCopyError) -> errno::Errno {
    match err {
        user::UserCopyError::NameTooLong => errno::ENAMETOOLONG,
        user::UserCopyError::TooLarge => errno::E2BIG,
        user::UserCopyError::OutOfMemory => errno::ENOMEM,
        _ => errno::EFAULT,
    }
}

fn exec_arg_copy_errno(err: user::UserCopyError) -> errno::Errno {
    match err {
        user::UserCopyError::NameTooLong | user::UserCopyError::TooLarge => errno::E2BIG,
        user::UserCopyError::OutOfMemory => errno::ENOMEM,
        _ => errno::EFAULT,
    }
}

fn wait_user_copy_errno(_err: user::UserCopyError) -> errno::Errno {
    errno::EFAULT
}

// Process terminal status is single-consumer. `process_consumer` claims that
// ownership before generic main-thread join, and consuming status/resources
// stays one process-table operation.
fn consume_process_for_join(
    pid: ProcessId,
    caller: ThreadId,
) -> Result<ReclaimedProcess, ConsumeProcessError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table
        .slot_mut(pid)
        .ok_or(ConsumeProcessError::Join(ProcessJoinError::InvalidProcess))?;

    if slot.process_consumer != Some(caller) {
        return Err(ConsumeProcessError::Join(ProcessJoinError::JoinInProgress));
    }

    let Some(status) = slot.exit_status.take() else {
        return Err(ConsumeProcessError::WouldBlock);
    };

    let reclaimed = ReclaimedProcess {
        status,
        address_space: slot.address_space.take(),
        user_image: slot.user_image.take(),
    };
    let next_generation = next_generation(slot.generation);
    table.release_slot(pid, next_generation);

    Ok(reclaimed)
}

fn cleanup_reclaimed_process(
    pid: ProcessId,
    reclaimed: ReclaimedProcess,
) -> Result<ProcessExitStatus, ProcessJoinError> {
    let status = reclaimed.status;
    if let Some(image) = reclaimed.user_image {
        elf::free_loaded_segments(&image);
    }
    if let Some(address_space) = reclaimed.address_space {
        // SAFETY: process join has observed terminal status, and the detached
        // main user thread has already exited, so no scheduler state references
        // this TTBR0 root anymore. User segment and stack frames have already
        // been returned to the frame allocator above.
        if let Err(err) = unsafe { vm::destroy_user_address_space(address_space) } {
            crate::warn!("process: failed to destroy address space pid={pid}: {err:?}");
        }
    }
    crate::debug!("process: reclaimed pid={pid} status={status:?}");
    Ok(status)
}

fn free_process_slot(pid: ProcessId) {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    if let Some(generation) = table.slot(pid).map(|slot| slot.generation) {
        table.release_slot(pid, generation);
    }
}

/// Return the current user process ramfs cwd directory index.
///
/// The process-table read runs in a short IRQ-disabled critical section.
///
/// # Returns
///
/// Returns the stable directory index stored in the current process.
///
/// # Errors
///
/// Returns `ProcessPathError::NoCurrentProcess` outside a user process, or
/// `ProcessPathError::InvalidDirectory` if process state lacks a cwd.
pub(crate) fn current_cwd_dir() -> Result<usize, ProcessPathError> {
    let pid = current_process_id().ok_or(ProcessPathError::NoCurrentProcess)?;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table
        .slot_mut(pid)
        .ok_or(ProcessPathError::NoCurrentProcess)?;
    slot.cwd_dir.ok_or(ProcessPathError::InvalidDirectory)
}

/// Return the current user process canonical absolute cwd pathname.
///
/// The stable ramfs lookup occurs after the process-table critical section.
///
/// # Returns
///
/// Returns a static slice borrowed from the immutable mounted ramfs index.
///
/// # Errors
///
/// Returns `ProcessPathError::NoCurrentProcess` outside a user process, or
/// `ProcessPathError::InvalidDirectory` for an invalid stored directory index.
pub(crate) fn current_cwd_path() -> Result<&'static [u8], ProcessPathError> {
    let cwd_dir = current_cwd_dir()?;
    ramfs::dir_path(cwd_dir).ok_or(ProcessPathError::InvalidDirectory)
}

/// Replace the current user process cwd with a ramfs directory index.
///
/// Directory validation happens before the short IRQ-disabled process-table
/// mutation. No allocation or pathname resolution occurs in the critical
/// section.
///
/// # Arguments
///
/// * `dir_index` - Stable index returned by `ramfs::lookup_dir()`.
///
/// # Returns
///
/// Returns `Ok(())` after updating the current process cwd.
///
/// # Errors
///
/// Returns `ProcessPathError::InvalidDirectory` for an invalid index, or
/// `ProcessPathError::NoCurrentProcess` outside a user process.
pub(crate) fn set_current_cwd(dir_index: usize) -> Result<(), ProcessPathError> {
    if ramfs::dir_path(dir_index).is_none() {
        return Err(ProcessPathError::InvalidDirectory);
    }

    let pid = current_process_id().ok_or(ProcessPathError::NoCurrentProcess)?;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table
        .slot_mut(pid)
        .ok_or(ProcessPathError::NoCurrentProcess)?;
    slot.cwd_dir = Some(dir_index);
    Ok(())
}

/// Open a ramfs regular file in the current user process FD table.
///
/// # Arguments
///
/// * `file_index` - Ramfs file index returned by `ramfs::lookup_file()`.
///
/// # Returns
///
/// Returns the allocated userspace descriptor number.
///
/// # Errors
///
/// Returns `FdError::BadFd` if there is no current user process and
/// `FdError::TooManyOpenFiles` if the current process FD table is full.
pub(crate) fn open_current_ram_file(file_index: usize) -> Result<usize, FdError> {
    with_current_fd_table_mut(|fds| fds.open_ram_file(file_index))
}

/// Open a ramfs directory in the current user process FD table.
///
/// # Arguments
///
/// * `dir_index` - Ramfs directory index returned by `ramfs::lookup_dir()`.
///
/// # Returns
///
/// Returns the allocated userspace descriptor number.
///
/// # Errors
///
/// Returns `FdError::BadFd` if there is no current user process and
/// `FdError::TooManyOpenFiles` if the current process FD table is full.
pub(crate) fn open_current_ram_dir(dir_index: usize) -> Result<usize, FdError> {
    with_current_fd_table_mut(|fds| fds.open_ram_dir(dir_index))
}

/// Borrow bytes at the current regular-file offset for a userspace `read()`.
///
/// The FD offset is advanced separately after the syscall layer successfully
/// copies the returned slice to userspace.
///
/// # Arguments
///
/// * `fd` - Descriptor in the current user process.
/// * `max_len` - Maximum bytes to borrow from the current regular-file offset.
///
/// # Returns
///
/// Returns a borrowed file-data slice of at most `max_len` bytes. The slice can
/// be empty at EOF.
///
/// # Errors
///
/// Returns `FdError::BadFd` if there is no current process or `fd` is invalid,
/// and propagates FD table errors such as `FdError::IsDirectory`.
pub(crate) fn current_fd_read_slice(fd: usize, max_len: usize) -> Result<&'static [u8], FdError> {
    with_current_fd_table(|fds| fds.read_slice(fd, max_len))
}

/// Advance the current process regular-file offset after a successful read.
///
/// # Arguments
///
/// * `fd` - Descriptor in the current user process.
/// * `amount` - Number of bytes successfully copied to userspace.
///
/// # Returns
///
/// Returns `Ok(())` after updating the descriptor offset.
///
/// # Errors
///
/// Returns `FdError::BadFd` if there is no current process or `fd` is invalid,
/// and propagates FD table errors such as `FdError::IsDirectory`.
pub(crate) fn advance_current_fd_read(fd: usize, amount: usize) -> Result<(), FdError> {
    with_current_fd_table_mut(|fds| fds.advance_read(fd, amount))
}

/// Borrow a directory entry relative to the current directory FD offset.
///
/// This read-only query lets `getdents64` encode one or more entries before
/// committing the directory offset.
///
/// # Arguments
///
/// * `fd` - Descriptor in the current user process.
/// * `relative_offset` - Entry offset relative to the descriptor's current
///   directory offset.
///
/// # Returns
///
/// Returns `Ok(Some(entry))` for an available entry and `Ok(None)` at
/// end-of-directory.
///
/// # Errors
///
/// Returns `FdError::BadFd` if there is no current process or `fd` is invalid,
/// and `FdError::NotDirectory` when `fd` is a regular-file handle.
pub(crate) fn current_fd_dir_entry_at(
    fd: usize,
    relative_offset: usize,
) -> Result<Option<crate::fs::fd::FdDirEntry>, FdError> {
    with_current_fd_table(|fds| fds.dir_entry_at(fd, relative_offset))
}

/// Advance the current process directory FD offset after successful `getdents64`.
///
/// # Arguments
///
/// * `fd` - Descriptor in the current user process.
/// * `amount` - Number of directory entries successfully copied to userspace.
///
/// # Returns
///
/// Returns `Ok(())` after updating the descriptor offset.
///
/// # Errors
///
/// Returns `FdError::BadFd` if there is no current process or `fd` is invalid,
/// and `FdError::NotDirectory` when `fd` is a regular-file handle.
pub(crate) fn advance_current_fd_dir_read(fd: usize, amount: usize) -> Result<(), FdError> {
    with_current_fd_table_mut(|fds| fds.advance_dir_read(fd, amount))
}

/// Close a descriptor in the current user process FD table.
///
/// # Arguments
///
/// * `fd` - Descriptor in the current user process.
///
/// # Returns
///
/// Returns `Ok(())` after clearing the descriptor.
///
/// # Errors
///
/// Returns `FdError::BadFd` if there is no current process, `fd` is reserved or
/// out of range, or the descriptor is already closed.
pub(crate) fn close_current_fd(fd: usize) -> Result<(), FdError> {
    with_current_fd_table_mut(|fds| fds.close(fd))
}

/// Return whether a descriptor is open in the current user process.
///
/// # Arguments
///
/// * `fd` - Descriptor in the current user process.
///
/// # Returns
///
/// Returns `Ok(true)` for an open user descriptor and `Ok(false)` for a closed
/// or reserved descriptor.
///
/// # Errors
///
/// Returns `FdError::BadFd` if there is no current user process.
pub(crate) fn current_fd_is_open(fd: usize) -> Result<bool, FdError> {
    with_current_fd_table(|fds| Ok(fds.is_open(fd)))
}

fn with_current_fd_table<T>(f: impl FnOnce(&FdTable) -> Result<T, FdError>) -> Result<T, FdError> {
    let pid = current_process_id().ok_or(FdError::BadFd)?;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table.slot_mut(pid).ok_or(FdError::BadFd)?;
    f(&slot.fds)
}

fn with_current_fd_table_mut<T>(
    f: impl FnOnce(&mut FdTable) -> Result<T, FdError>,
) -> Result<T, FdError> {
    // FD table sharing across multiple user threads in one process is not
    // implemented yet. These short IRQ-disabled sections are sufficient for the
    // current single-threaded user process milestone; user memory copies happen
    // outside them in the syscall layer.
    let pid = current_process_id().ok_or(FdError::BadFd)?;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table.slot_mut(pid).ok_or(FdError::BadFd)?;
    f(&mut slot.fds)
}

fn take_process_resources(pid: ProcessId) -> ProcessResources {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let Some(slot) = table.slot_mut(pid) else {
        return ProcessResources {
            address_space: None,
            user_image: None,
        };
    };

    let resources = ProcessResources {
        address_space: slot.address_space.take(),
        user_image: slot.user_image.take(),
    };
    resources
}

fn load_init_image(
    address_space: &mut OwnedUserAddressSpace,
) -> Result<UserElfImage, ProcessError> {
    let image = initramfs::init_file().map_err(ProcessError::Initramfs)?;
    crate::info!("init: loading /init from initramfs");
    crate::info!("init: /init size={}", image.len());
    elf::load_user_elf(image, address_space).map_err(ProcessError::Elf)
}

impl ProcessTable {
    fn slot(&self, pid: ProcessId) -> Option<&ProcessSlot> {
        let slot = self.slots.get(pid.index())?;
        (slot.generation == pid.generation() && !slot.is_free()).then_some(slot)
    }

    fn slot_mut(&mut self, pid: ProcessId) -> Option<&mut ProcessSlot> {
        let slot = self.slots.get_mut(pid.index())?;
        (slot.generation == pid.generation() && !slot.is_free()).then_some(slot)
    }

    fn process_for_thread(&self, thread: ThreadId) -> Option<ProcessId> {
        let pid = *self.thread_owners.get(thread.index())?.as_ref()?;
        let slot = self
            .slot(pid)
            .unwrap_or_else(|| panic!("process: stale reverse owner {pid} for thread {thread}"));
        if slot.main_thread != Some(thread) {
            panic!("process: reverse owner mismatch pid={pid} thread={thread}");
        }
        Some(pid)
    }

    fn bind_main_thread(&mut self, pid: ProcessId, thread: ThreadId) {
        if thread.index() >= self.thread_owners.len() {
            panic!("process: main thread index outside reverse owner table: {thread}");
        }
        if self.thread_owners[thread.index()].is_some() {
            panic!("process: main thread already belongs to a process: {thread}");
        }
        let slot = self
            .slot_mut(pid)
            .unwrap_or_else(|| panic!("process: published thread lost slot {pid}"));
        if slot.main_thread.is_some() {
            panic!("process: pid={pid} already has a main thread");
        }
        slot.main_thread = Some(thread);
        self.thread_owners[thread.index()] = Some(pid);
    }

    fn unbind_main_thread(&mut self, pid: ProcessId) {
        let thread = self
            .slot(pid)
            .unwrap_or_else(|| panic!("process: unbind of invalid slot {pid}"))
            .main_thread
            .unwrap_or_else(|| panic!("process: pid={pid} has no main thread to unbind"));
        let owner = self
            .thread_owners
            .get_mut(thread.index())
            .unwrap_or_else(|| panic!("process: main thread index outside reverse owner table"));
        if *owner != Some(pid) {
            panic!("process: reverse owner missing during unbind pid={pid} thread={thread}");
        }
        *owner = None;
    }

    fn release_slot(&mut self, pid: ProcessId, generation: u32) {
        let main_thread = self
            .slot(pid)
            .unwrap_or_else(|| panic!("process: release of invalid slot {pid}"))
            .main_thread;
        if let Some(thread) = main_thread {
            let owner = self
                .thread_owners
                .get_mut(thread.index())
                .unwrap_or_else(|| {
                    panic!("process: main thread index outside reverse owner table")
                });
            // A terminal process unbinds before generic thread reap. Its slot
            // may therefore have been reused and rebound before this later
            // process reclaim; never erase the newer generation's owner.
            if *owner == Some(pid) {
                *owner = None;
            }
        }
        self.slots[pid.index()] = ProcessSlot {
            generation,
            ..ProcessSlot::free()
        };
    }
}

fn next_generation(generation: u32) -> u32 {
    let next = generation.wrapping_add(1);
    if next == 0 {
        INITIAL_PROCESS_GENERATION
    } else {
        next
    }
}

fn table_mut() -> &'static mut ProcessTable {
    // SAFETY: single-core access discipline is documented on `ProcessTableCell`.
    unsafe { &mut *PROCESS_TABLE.0.get() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occupy_process_slot(table: &mut ProcessTable, index: usize, generation: u32) -> ProcessId {
        let pid = ProcessId::new(index, generation);
        table.slots[index] = ProcessSlot {
            generation,
            state: ProcessState::Running,
            ..ProcessSlot::free()
        };
        pid
    }

    #[test]
    fn reverse_thread_owner_survives_thread_slot_reuse_before_process_reclaim() {
        let mut table = ProcessTable::new();
        let old_pid = occupy_process_slot(&mut table, 0, INITIAL_PROCESS_GENERATION);
        let old_thread = ThreadId::new(KERNEL_THREAD_CAPACITY - 1, 7);

        table.bind_main_thread(old_pid, old_thread);
        assert_eq!(table.process_for_thread(old_thread), Some(old_pid));
        assert_eq!(
            table.process_for_thread(ThreadId::new(old_thread.index() - 1, 7)),
            None
        );

        table.unbind_main_thread(old_pid);
        assert_eq!(table.process_for_thread(old_thread), None);

        let new_pid = occupy_process_slot(&mut table, 1, INITIAL_PROCESS_GENERATION);
        let new_thread = ThreadId::new(old_thread.index(), old_thread.generation() + 1);
        table.bind_main_thread(new_pid, new_thread);

        table.release_slot(old_pid, next_generation(old_pid.generation()));
        assert_eq!(table.process_for_thread(new_thread), Some(new_pid));
    }
}
