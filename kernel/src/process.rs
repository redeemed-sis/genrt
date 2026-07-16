use alloc::vec::Vec;
use core::{cell::UnsafeCell, fmt, mem};

use crate::{
    arch::ActiveContext,
    errno,
    fs::fd::{FdError, FdTable},
    fs::initramfs::{self, InitramfsError},
    fs::{path, ramfs},
    loader::elf::{self, ElfLoadError, UserElfImage, UserElfSegment},
    memory::{
        self, FrameRange, PAGE_SIZE,
        user::{self, USER_STACK_TOP},
        vm::{self, UserAddressSpace, UserMapFlags, VmError},
    },
    sched::{self, ThreadAttrs},
    sync::LocalIrqGuard,
    task::ThreadId,
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
    address_space: Option<UserAddressSpace>,
    user_image: Option<UserElfImage>,
    fds: FdTable,
    cwd_dir: Option<usize>,
    parent: Option<ProcessId>,
    main_thread: Option<ThreadId>,
    stack: FrameRange,
    exit_status: Option<ProcessExitStatus>,
    joiner: Option<ThreadId>,
    waiter: Option<ThreadId>,
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
            stack: FrameRange::empty(),
            exit_status: None,
            joiner: None,
            waiter: None,
        }
    }

    fn is_free(&self) -> bool {
        self.state == ProcessState::Free
    }
}

struct ProcessTable {
    slots: [ProcessSlot; MAX_PROCESSES],
}

struct ReclaimedProcess {
    status: ProcessExitStatus,
    address_space: Option<UserAddressSpace>,
    user_image: Option<UserElfImage>,
    stack: FrameRange,
}

struct ProcessResources {
    address_space: Option<UserAddressSpace>,
    user_image: Option<UserElfImage>,
    stack: FrameRange,
}

struct StagedProcessImage {
    address_space: UserAddressSpace,
    user_image: UserElfImage,
    stack: FrameRange,
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
    address_space: Option<UserAddressSpace>,
    user_image: Option<UserElfImage>,
    stack: FrameRange,
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
        }
    }
}

struct ProcessTableCell(UnsafeCell<ProcessTable>);

// SAFETY: genrt is single-core for this milestone. Process table mutations run
// either in short IRQ-disabled task-context sections or in the synchronous
// lower-EL trap path for the currently running task.
unsafe impl Sync for ProcessTableCell {}

static PROCESS_TABLE: ProcessTableCell = ProcessTableCell(UnsafeCell::new(ProcessTable::new()));

pub(crate) fn spawn_first_user_process() -> Result<ProcessId, ProcessError> {
    let cwd_dir = ramfs::root_dir_index().ok_or(ProcessError::InvalidProcess)?;
    let pid = allocate_process_slot(None, FdTable::new(), cwd_dir)?;
    let address_space = vm::create_user_address_space().map_err(|err| {
        free_process_slot(pid);
        ProcessError::Vm(err)
    })?;
    let mut stack = FrameRange::empty();
    let mut user_image = None;

    let result = (|| {
        let loaded = load_init_image(address_space)?;
        let entry = loaded.entry;
        user_image = Some(loaded);

        stack = memory::alloc_contiguous_frames(USER_STACK_SIZE / PAGE_SIZE)
            .ok_or(ProcessError::OutOfFrames)?;
        memory::zero_phys_range(stack);
        map_user_stack(address_space, stack)?;
        let mut exec_args = ExecArgs::empty();
        exec_args
            .push(ExecStringVector::Argv, b"/init".to_vec())
            .map_err(|_| ProcessError::OutOfFrames)?;
        let initial_sp =
            build_initial_user_stack(stack, &exec_args).ok_or(ProcessError::OutOfFrames)?;
        attach_process_resources(
            pid,
            address_space,
            user_image.take().ok_or(ProcessError::InvalidProcess)?,
            stack,
        )?;

        {
            let _irq_guard = LocalIrqGuard::save_and_disable();
            let main_thread = sched::thread_spawn_user(
                pid,
                address_space,
                entry,
                initial_sp,
                0,
                ThreadAttrs::detached(),
            )
            .map_err(ProcessError::Spawn)?;
            attach_process_main_thread(pid, main_thread)?;
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
        if attached.stack.start != 0 {
            memory::free_contiguous_frames(attached.stack);
        } else if stack.start != 0 {
            memory::free_contiguous_frames(stack);
        }
        let address_space = attached.address_space.unwrap_or(address_space);
        // SAFETY: a failed spawn path never leaves a runnable user thread that
        // can still reference this address space.
        let _ = unsafe { vm::destroy_user_address_space(address_space) };
        free_process_slot(pid);
    }

    result
}

pub(crate) fn process_join(pid: ProcessId) -> Result<ProcessExitStatus, ProcessJoinError> {
    if sched::current_user_process_id() == Some(pid) {
        return Err(ProcessJoinError::SelfJoin);
    }

    let caller = sched::current_thread_id().ok_or(ProcessJoinError::SchedulerNotInitialized)?;
    match consume_process_for_join(pid, caller) {
        Ok(reclaimed) => return cleanup_reclaimed_process(pid, reclaimed),
        Err(ConsumeProcessError::WouldBlock) => {}
        Err(ConsumeProcessError::Join(err)) => return Err(err),
    }

    crate::task_call::process_join(pid);

    let reclaimed = match consume_process_for_join(pid, caller) {
        Ok(reclaimed) => reclaimed,
        Err(ConsumeProcessError::WouldBlock) => {
            return Err(ProcessJoinError::InvalidProcess);
        }
        Err(ConsumeProcessError::Join(err)) => return Err(err),
    };
    cleanup_reclaimed_process(pid, reclaimed)
}

/// Register and block the current kernel task waiting for a process to finish.
///
/// # Arguments
///
/// * `context` - Exclusive live task-call context for scheduler handoff.
/// * `pid` - Generation-checked process whose terminal state is awaited.
///
/// # Returns
///
/// Returns without blocking when registration is invalid or unnecessary;
/// otherwise returns after the scheduler later resumes the waiter. The
/// registration and handoff are bounded and do not allocate.
///
/// # Panics
///
/// Panics when a [`crate::sync::preempt::PreemptGuard`] is active before publishing the
/// process joiner.
pub(crate) fn on_process_join_sync(context: &mut ActiveContext<'_>, pid: ProcessId) {
    let Some(joiner) = sched::current_thread_id() else {
        return;
    };

    {
        let table = table_mut();
        let Some(slot) = table.slot_mut(pid) else {
            return;
        };

        if slot.exit_status.is_some() {
            return;
        }
        if slot.joiner.is_some() {
            return;
        }

        crate::sync::preempt::assert_preemption_enabled("process joiner publication");
        slot.joiner = Some(joiner);
    }

    sched::block_current_on_process_wait(context, pid);
    crate::debug!("process: join blocking current={joiner} pid={pid}");
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
/// with another runnable task; the exiting userspace thread does not resume.
/// The terminal transition is bounded and does not allocate.
///
/// # Panics
///
/// Panics if no current process can own the exit or a
/// [`crate::sync::preempt::PreemptGuard`] is active before terminal state changes.
pub(crate) fn process_exit_current(context: &mut ActiveContext<'_>, code: usize) {
    // Safe point: a terminal process handoff cannot publish exit state while a
    // task-only preemption guard still owns its protected access.
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
/// This task-context operation may allocate and copy process-owned memory. Only
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
pub(crate) fn fork_current(context: &mut ActiveContext<'_>) -> Result<usize, errno::Errno> {
    let snapshot = fork_snapshot_current()?;
    let child_pid =
        allocate_process_slot(Some(snapshot.parent_pid), snapshot.fds, snapshot.cwd_dir)
            .map_err(fork_errno)?;
    let mut address_space = None;
    let mut user_image = None;
    let mut stack = FrameRange::empty();

    let result = (|| {
        let child_aspace = vm::create_user_address_space().map_err(fork_vm_errno)?;
        address_space = Some(child_aspace);

        let child_image = clone_user_image(snapshot.user_image(), child_aspace)?;
        user_image = Some(child_image);

        stack = clone_user_stack(snapshot.stack, child_aspace)?;
        attach_process_resources(
            child_pid,
            child_aspace,
            user_image.take().ok_or(errno::ENOMEM)?,
            stack,
        )
        .map_err(fork_errno)?;

        {
            let _irq_guard = LocalIrqGuard::save_and_disable();
            let child_thread = sched::thread_spawn_user_from_context(
                child_pid,
                child_aspace,
                context,
                ThreadAttrs::detached(),
            )
            .map_err(spawn_errno)?;
            attach_process_main_thread(child_pid, child_thread).unwrap_or_else(|err| {
                panic!("process: child thread published without process slot: {err:?}")
            });
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
        if attached.stack.start != 0 {
            memory::free_contiguous_frames(attached.stack);
        } else if stack.start != 0 {
            memory::free_contiguous_frames(stack);
        }
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
/// This task-context operation may allocate, parse ELF data, and copy process
/// memory. Those operations occur outside scheduler and IRQ fast paths.
pub(crate) fn execve_current(
    context: &mut ActiveContext<'_>,
    path: path::ResolvedPath,
    args: ExecArgs,
) -> Result<(), errno::Errno> {
    let pid = sched::current_user_process_id().ok_or(errno::EINVAL)?;
    let staged = stage_exec_image(&path, &args)?;
    let old = commit_exec_image(pid, staged, context)?;
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

    let parent_pid = match sched::current_user_process_id() {
        Some(pid) => pid,
        None => return WaitPidAction::Return(Err(errno::EINVAL)),
    };
    let caller = match sched::current_thread_id() {
        Some(thread) => thread,
        None => return WaitPidAction::Return(Err(errno::EINVAL)),
    };

    let prepare = {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        let prepare = prepare_wait_child_locked(parent_pid, child_pid, caller);
        if matches!(prepare, WaitChildPrepare::WouldBlock) {
            // waitpid blocks before producing userspace-visible side effects.
            // The architecture facade restarts the syscall after wake.
            context.restart_current_syscall();
            sched::block_current_on_process_wait(context, child_pid);
        }
        prepare
    };

    match prepare {
        WaitChildPrepare::Consumed(waited) => {
            let encoded = encode_wait_status(waited.status);
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
        WaitChildPrepare::WouldBlock => WaitPidAction::Blocked,
        WaitChildPrepare::Err(errno) => WaitPidAction::Return(Err(errno)),
    }
}

fn finish_current_process(
    context: &mut ActiveContext<'_>,
    status: ProcessExitStatus,
    thread_code: usize,
) -> Result<(), ProcessFaultError> {
    crate::sync::preempt::assert_preemption_enabled("process terminal state change");
    let pid = sched::current_user_process_id().ok_or(ProcessFaultError::NoCurrentProcess)?;
    let thread = sched::current_thread_id();
    let wake = finish_process(pid, status).ok_or(ProcessFaultError::InvalidProcess)?;

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

    if let Some(joiner) = wake.joiner {
        sched::complete_process_wait(joiner, pid);
    }
    if let Some(waiter) = wake.waiter {
        sched::complete_process_wait(waiter, pid);
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
        stack: FrameRange::empty(),
        exit_status: None,
        joiner: None,
        waiter: None,
    };

    let pid = ProcessId::new(index, generation);
    crate::debug!("process: allocated slot pid={pid}");
    Ok(pid)
}

fn attach_process_resources(
    pid: ProcessId,
    address_space: UserAddressSpace,
    user_image: UserElfImage,
    stack: FrameRange,
) -> Result<(), ProcessError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table.slot_mut(pid).ok_or(ProcessError::InvalidProcess)?;
    slot.address_space = Some(address_space);
    slot.user_image = Some(user_image);
    slot.stack = stack;
    slot.state = ProcessState::Running;
    crate::debug!(
        "process: pid={pid} resources ready ttbr0=0x{:x}",
        address_space.root_pa()
    );
    Ok(())
}

fn attach_process_main_thread(pid: ProcessId, main_thread: ThreadId) -> Result<(), ProcessError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table.slot_mut(pid).ok_or(ProcessError::InvalidProcess)?;
    slot.main_thread = Some(main_thread);
    crate::debug!("process: pid={pid} running main={main_thread}");
    Ok(())
}

#[derive(Copy, Clone)]
struct ProcessWake {
    joiner: Option<ThreadId>,
    waiter: Option<ThreadId>,
}

fn finish_process(pid: ProcessId, status: ProcessExitStatus) -> Option<ProcessWake> {
    let table = table_mut();
    let slot = table.slot_mut(pid)?;
    if slot.exit_status.is_some() {
        return Some(ProcessWake {
            joiner: None,
            waiter: None,
        });
    }

    slot.fds.close_all();
    slot.cwd_dir = None;
    slot.state = ProcessState::Zombie;
    slot.exit_status = Some(status);
    let joiner = slot.joiner;
    let waiter = slot.waiter;
    Some(ProcessWake { joiner, waiter })
}

struct ForkSnapshot {
    parent_pid: ProcessId,
    user_image: *const UserElfImage,
    stack: FrameRange,
    fds: FdTable,
    cwd_dir: usize,
}

impl ForkSnapshot {
    fn user_image(&self) -> &UserElfImage {
        // SAFETY: the pointer targets the current process slot. genrt currently
        // has one user thread per process, so the current process cannot mutate
        // or reclaim its own image while fork clones resources in task context.
        unsafe { &*self.user_image }
    }
}

enum WaitChildPrepare {
    Consumed(WaitedProcess),
    WouldBlock,
    Err(errno::Errno),
}

fn fork_snapshot_current() -> Result<ForkSnapshot, errno::Errno> {
    let parent_pid = sched::current_user_process_id().ok_or(errno::EINVAL)?;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table.slot_mut(parent_pid).ok_or(errno::EINVAL)?;
    let user_image = slot.user_image.as_ref().ok_or(errno::EINVAL)? as *const UserElfImage;
    let cwd_dir = slot.cwd_dir.ok_or(errno::EINVAL)?;

    Ok(ForkSnapshot {
        parent_pid,
        user_image,
        stack: slot.stack,
        fds: slot.fds,
        cwd_dir,
    })
}

fn clone_user_image(
    src: &UserElfImage,
    dst_aspace: UserAddressSpace,
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
    dst_aspace: UserAddressSpace,
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

fn clone_user_stack(
    src_stack: FrameRange,
    dst_aspace: UserAddressSpace,
) -> Result<FrameRange, errno::Errno> {
    let stack = memory::clone_frame_range(src_stack).map_err(clone_frame_errno)?;
    if let Err(err) = map_user_stack(dst_aspace, stack) {
        memory::free_contiguous_frames(stack);
        return Err(fork_errno(err));
    }
    Ok(stack)
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

    let address_space = vm::create_user_address_space().map_err(exec_vm_errno)?;
    let mut stack = FrameRange::empty();
    let mut user_image = None;

    let result = (|| {
        let mut aspace = address_space;
        let loaded = elf::load_user_elf(image, &mut aspace).map_err(exec_elf_errno)?;
        let entry = loaded.entry;
        user_image = Some(loaded);

        stack =
            memory::alloc_contiguous_frames(USER_STACK_SIZE / PAGE_SIZE).ok_or(errno::ENOMEM)?;
        memory::zero_phys_range(stack);
        map_user_stack(address_space, stack).map_err(fork_errno)?;
        let initial_sp = build_initial_user_stack(stack, args).ok_or(errno::E2BIG)?;

        Ok(StagedProcessImage {
            address_space,
            user_image: user_image.take().ok_or(errno::ENOMEM)?,
            stack,
            entry,
            initial_sp,
        })
    })();

    if result.is_err() {
        if let Some(image) = user_image {
            elf::free_loaded_segments(&image);
        }
        if stack.start != 0 {
            memory::free_contiguous_frames(stack);
        }
        // SAFETY: staged exec has not been published to the scheduler/process
        // table yet, so no running task can reference this root.
        let _ = unsafe { vm::destroy_user_address_space(address_space) };
    }

    result
}

fn commit_exec_image(
    pid: ProcessId,
    staged: StagedProcessImage,
    context: &mut ActiveContext<'_>,
) -> Result<ProcessResources, errno::Errno> {
    let entry = staged.entry;
    let initial_sp = staged.initial_sp;
    sched::replace_current_user_address_space(staged.address_space).map_err(|_| errno::EINVAL)?;

    let old = replace_process_resources(pid, staged.address_space, staged.user_image, staged.stack)
        .ok_or(errno::EINVAL)?;

    // The current thread now points at the new TTBR0 root and the stack was
    // populated through HVA before commit. The architecture facade preserves
    // the EL1 kernel stack while replacing all EL0-visible state.
    context.replace_user_context_after_exec(entry, initial_sp);

    Ok(old)
}

fn replace_process_resources(
    pid: ProcessId,
    address_space: UserAddressSpace,
    user_image: UserElfImage,
    stack: FrameRange,
) -> Option<ProcessResources> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table.slot_mut(pid)?;
    let old = ProcessResources {
        address_space: slot.address_space.replace(address_space),
        user_image: slot.user_image.replace(user_image),
        stack: mem::replace(&mut slot.stack, stack),
    };
    slot.state = ProcessState::Running;
    slot.exit_status = None;
    Some(old)
}

fn cleanup_process_resources(pid: ProcessId, resources: ProcessResources) {
    if let Some(image) = resources.user_image {
        elf::free_loaded_segments(&image);
    }
    if resources.stack.start != 0 {
        memory::free_contiguous_frames(resources.stack);
    }
    if let Some(address_space) = resources.address_space {
        // SAFETY: resources were atomically removed from the process slot and
        // scheduler before cleanup, so no runnable thread references this root.
        if let Err(err) = unsafe { vm::destroy_user_address_space(address_space) } {
            crate::warn!("process: failed to destroy old address space pid={pid}: {err:?}");
        }
    }
}

fn build_initial_user_stack(stack: FrameRange, args: &ExecArgs) -> Option<usize> {
    let stack_base = USER_STACK_TOP.checked_sub(USER_STACK_SIZE)?;
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
    stack: FrameRange,
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
    stack: FrameRange,
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
    memory::copy_bytes_to_phys(stack.start + offset, bytes);
    Some(())
}

fn write_stack_byte(stack: FrameRange, stack_base: usize, user_va: usize, byte: u8) -> Option<()> {
    let offset = user_va.checked_sub(stack_base)?;
    if offset >= USER_STACK_SIZE {
        return None;
    }
    memory::copy_bytes_to_phys(stack.start + offset, &[byte]);
    Some(())
}

fn write_stack_u64(stack: FrameRange, stack_base: usize, user_va: usize, value: u64) -> Option<()> {
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
    if let Some(waiter) = slot.waiter {
        if waiter != caller {
            return WaitChildPrepare::Err(errno::ECHILD);
        }
    }

    let Some(status) = slot.exit_status.take() else {
        crate::sync::preempt::assert_preemption_enabled("process waiter publication");
        slot.waiter = Some(caller);
        return WaitChildPrepare::WouldBlock;
    };

    let waited = WaitedProcess {
        pid: child_pid,
        status,
        address_space: slot.address_space.take(),
        user_image: slot.user_image.take(),
        stack: slot.stack,
    };
    let next_generation = next_generation(slot.generation);
    *slot = ProcessSlot {
        generation: next_generation,
        ..ProcessSlot::free()
    };
    WaitChildPrepare::Consumed(waited)
}

fn cleanup_waited_process(waited: WaitedProcess) {
    cleanup_process_resources(
        waited.pid,
        ProcessResources {
            address_space: waited.address_space,
            user_image: waited.user_image,
            stack: waited.stack,
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
        sched::SpawnError::SchedulerNotInitialized => errno::EINVAL,
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

// Process terminal status is single-consumer. A terminal process may be
// reclaimed only by the registered joiner, if one exists, or by the first
// thread that atomically consumes an unclaimed terminal process. Reading status
// and reclaiming resources must stay one process-table operation.
fn consume_process_for_join(
    pid: ProcessId,
    caller: ThreadId,
) -> Result<ReclaimedProcess, ConsumeProcessError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table
        .slot_mut(pid)
        .ok_or(ConsumeProcessError::Join(ProcessJoinError::InvalidProcess))?;

    if let Some(joiner) = slot.joiner {
        if joiner != caller {
            return Err(ConsumeProcessError::Join(ProcessJoinError::JoinInProgress));
        }
    }

    let Some(status) = slot.exit_status.take() else {
        return Err(ConsumeProcessError::WouldBlock);
    };

    let reclaimed = ReclaimedProcess {
        status,
        address_space: slot.address_space.take(),
        user_image: slot.user_image.take(),
        stack: slot.stack,
    };
    let next_generation = next_generation(slot.generation);
    *slot = ProcessSlot {
        generation: next_generation,
        ..ProcessSlot::free()
    };

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
    if reclaimed.stack.start != 0 {
        memory::free_contiguous_frames(reclaimed.stack);
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
    if let Some(slot) = table.slot_mut(pid) {
        *slot = ProcessSlot {
            generation: slot.generation,
            ..ProcessSlot::free()
        };
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
    let pid = sched::current_user_process_id().ok_or(ProcessPathError::NoCurrentProcess)?;
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

    let pid = sched::current_user_process_id().ok_or(ProcessPathError::NoCurrentProcess)?;
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
    let pid = sched::current_user_process_id().ok_or(FdError::BadFd)?;
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
    let pid = sched::current_user_process_id().ok_or(FdError::BadFd)?;
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
            stack: FrameRange::empty(),
        };
    };

    let resources = ProcessResources {
        address_space: slot.address_space.take(),
        user_image: slot.user_image.take(),
        stack: slot.stack,
    };
    slot.stack = FrameRange::empty();
    resources
}

fn load_init_image(address_space: UserAddressSpace) -> Result<UserElfImage, ProcessError> {
    let image = initramfs::init_file().map_err(ProcessError::Initramfs)?;
    crate::info!("init: loading /init from initramfs");
    crate::info!("init: /init size={}", image.len());
    let mut address_space = address_space;
    elf::load_user_elf(image, &mut address_space).map_err(ProcessError::Elf)
}

fn map_user_stack(address_space: UserAddressSpace, stack: FrameRange) -> Result<(), ProcessError> {
    let stack_base = USER_STACK_TOP - USER_STACK_SIZE;
    crate::debug!(
        "process: map user stack pa={:?} va=0x{:x}..0x{:x}",
        stack,
        stack_base,
        USER_STACK_TOP
    );
    vm::map_user_page_range(
        address_space,
        stack_base,
        stack.start,
        USER_STACK_SIZE,
        UserMapFlags::WRITE,
    )
    .map_err(ProcessError::Vm)
}

impl ProcessTable {
    fn slot_mut(&mut self, pid: ProcessId) -> Option<&mut ProcessSlot> {
        let slot = self.slots.get_mut(pid.index())?;
        (slot.generation == pid.generation() && !slot.is_free()).then_some(slot)
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
