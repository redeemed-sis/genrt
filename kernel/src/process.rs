use core::{cell::UnsafeCell, fmt};

use crate::{
    fs::fd::{FdError, FdTable},
    fs::initramfs::{self, InitramfsError},
    loader::elf::{self, ElfLoadError, UserElfImage},
    memory::{
        self, FrameRange, PAGE_SIZE,
        user::USER_STACK_TOP,
        vm::{self, UserAddressSpace, UserMapFlags, VmError},
    },
    sched::{self, ThreadAttrs},
    sync::LocalIrqGuard,
    task::ThreadId,
};

pub(crate) const USER_STACK_SIZE: usize = 64 * 1024;

const MAX_PROCESSES: usize = 4;
const INITIAL_PROCESS_GENERATION: u32 = 1;

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
    Exited,
    Faulted,
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
pub enum UserFaultKind {
    UnknownSyscall(usize),
    InstructionAbort,
    DataAbort,
    PermissionFault,
    TranslationFault,
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

struct ProcessSlot {
    generation: u32,
    state: ProcessState,
    address_space: Option<UserAddressSpace>,
    user_image: Option<UserElfImage>,
    fds: FdTable,
    main_thread: Option<ThreadId>,
    stack: FrameRange,
    exit_status: Option<ProcessExitStatus>,
    joiner: Option<ThreadId>,
}

impl ProcessSlot {
    const fn free() -> Self {
        Self {
            generation: 0,
            state: ProcessState::Free,
            address_space: None,
            user_image: None,
            fds: FdTable::new(),
            main_thread: None,
            stack: FrameRange::empty(),
            exit_status: None,
            joiner: None,
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
    let pid = allocate_process_slot()?;
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
                USER_STACK_TOP,
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

pub(crate) fn on_process_join_sync(active_frame_words: *mut u64, pid: ProcessId) {
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

        slot.joiner = Some(joiner);
    }

    sched::block_current_on_process_join(active_frame_words, pid);
    crate::debug!("process: join blocking current={joiner} pid={pid}");
}

pub(crate) fn process_exit_current(active_frame_words: *mut u64, code: usize) {
    finish_current_process(active_frame_words, ProcessExitStatus::Exited(code), code)
        .unwrap_or_else(|err| panic!("process: sys_exit without current process: {err:?}"));
}

pub fn kill_current_process_on_user_fault(
    active_frame_words: *mut u64,
    fault: UserFault,
) -> Result<(), ProcessFaultError> {
    finish_current_process(
        active_frame_words,
        ProcessExitStatus::Faulted(fault),
        usize::MAX,
    )
}

fn finish_current_process(
    active_frame_words: *mut u64,
    status: ProcessExitStatus,
    thread_code: usize,
) -> Result<(), ProcessFaultError> {
    let pid = sched::current_user_process_id().ok_or(ProcessFaultError::NoCurrentProcess)?;
    let thread = sched::current_thread_id();
    let joiner = finish_process(pid, status).ok_or(ProcessFaultError::InvalidProcess)?;

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

    if let Some(joiner) = joiner {
        sched::complete_process_join(joiner, pid);
    }
    sched::on_thread_exit_sync(active_frame_words, thread_code);
    Ok(())
}

fn allocate_process_slot() -> Result<ProcessId, ProcessError> {
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
        fds: FdTable::new(),
        main_thread: None,
        stack: FrameRange::empty(),
        exit_status: None,
        joiner: None,
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

fn finish_process(pid: ProcessId, status: ProcessExitStatus) -> Option<Option<ThreadId>> {
    let table = table_mut();
    let slot = table.slot_mut(pid)?;
    if slot.exit_status.is_some() {
        return Some(None);
    }

    slot.fds.close_all();
    slot.state = match status {
        ProcessExitStatus::Exited(_) => ProcessState::Exited,
        ProcessExitStatus::Faulted(_) => ProcessState::Faulted,
    };
    slot.exit_status = Some(status);
    let joiner = slot.joiner;
    if joiner.is_some() {
        slot.state = ProcessState::Zombie;
    }
    Some(joiner)
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

pub(crate) fn open_current_ram_file(file_index: usize) -> Result<usize, FdError> {
    with_current_fd_table_mut(|fds| fds.open_ram_file(file_index))
}

pub(crate) fn current_fd_read_slice(fd: usize, max_len: usize) -> Result<&'static [u8], FdError> {
    with_current_fd_table(|fds| fds.read_slice(fd, max_len))
}

pub(crate) fn advance_current_fd_read(fd: usize, amount: usize) -> Result<(), FdError> {
    with_current_fd_table_mut(|fds| fds.advance_read(fd, amount))
}

pub(crate) fn close_current_fd(fd: usize) -> Result<(), FdError> {
    with_current_fd_table_mut(|fds| fds.close(fd))
}

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
