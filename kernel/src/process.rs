use core::{cell::UnsafeCell, fmt};

use crate::{
    memory::{
        self, FrameRange, PAGE_SIZE, PhysAddr, VirtAddr,
        user::{USER_STACK_TOP, USER_TEXT_BASE},
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

#[derive(Copy, Clone)]
struct ProcessSlot {
    generation: u32,
    state: ProcessState,
    address_space: Option<UserAddressSpace>,
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
            main_thread: None,
            stack: FrameRange::empty(),
            exit_status: None,
            joiner: None,
        }
    }

    fn is_free(self) -> bool {
        self.state == ProcessState::Free
    }
}

struct ProcessTable {
    slots: [ProcessSlot; MAX_PROCESSES],
}

struct ReclaimedProcess {
    status: ProcessExitStatus,
    address_space: Option<UserAddressSpace>,
    stack: FrameRange,
}

enum ConsumeProcessError {
    WouldBlock,
    Join(ProcessJoinError),
}

impl ProcessTable {
    const fn new() -> Self {
        Self {
            slots: [ProcessSlot::free(); MAX_PROCESSES],
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

    let result = (|| {
        map_user_image(address_space)?;

        stack = memory::alloc_contiguous_frames(USER_STACK_SIZE / PAGE_SIZE)
            .ok_or(ProcessError::OutOfFrames)?;
        zero_phys_range(stack);
        map_user_stack(address_space, stack)?;
        attach_process_resources(pid, address_space, stack)?;

        {
            let _irq_guard = LocalIrqGuard::save_and_disable();
            let main_thread = sched::thread_spawn_user(
                pid,
                address_space,
                USER_TEXT_BASE,
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
        if stack.start != 0 {
            memory::free_contiguous_frames(stack);
        }
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
    stack: FrameRange,
) -> Result<(), ProcessError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table.slot_mut(pid).ok_or(ProcessError::InvalidProcess)?;
    slot.address_space = Some(address_space);
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
    if reclaimed.stack.start != 0 {
        memory::free_contiguous_frames(reclaimed.stack);
    }
    if let Some(address_space) = reclaimed.address_space {
        // SAFETY: process join has observed terminal status, and the detached
        // main user thread has already exited, so no scheduler state references
        // this TTBR0 root anymore. Leaf user image frames belong to the QEMU
        // loader reservation and are not freed here; the stack frames are freed
        // separately above.
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

fn map_user_image(address_space: UserAddressSpace) -> Result<(), ProcessError> {
    let image = vm::user_image_load_range();
    let size = vm::user_image_bringup_size();
    crate::debug!(
        "process: map user image pa=0x{:x} size={} va=0x{:x}",
        image.start,
        size,
        USER_TEXT_BASE
    );
    map_page_range(
        address_space,
        USER_TEXT_BASE,
        image.start,
        size,
        UserMapFlags::EXECUTE,
    )
}

fn map_user_stack(address_space: UserAddressSpace, stack: FrameRange) -> Result<(), ProcessError> {
    let stack_base = USER_STACK_TOP - USER_STACK_SIZE;
    crate::debug!(
        "process: map user stack pa={:?} va=0x{:x}..0x{:x}",
        stack,
        stack_base,
        USER_STACK_TOP
    );
    map_page_range(
        address_space,
        stack_base,
        stack.start,
        USER_STACK_SIZE,
        UserMapFlags::WRITE,
    )
}

fn map_page_range(
    address_space: UserAddressSpace,
    va: VirtAddr,
    pa: PhysAddr,
    size: usize,
    flags: UserMapFlags,
) -> Result<(), ProcessError> {
    let mut offset = 0usize;
    while offset < size {
        // SAFETY: caller provides page-aligned process layout ranges and the
        // TTBR0 root is owned by this process until `process_join()` reclaims it.
        unsafe {
            vm::map_user_page(address_space, va + offset, pa + offset, flags)
                .map_err(ProcessError::Vm)?;
        }
        offset += PAGE_SIZE;
    }
    Ok(())
}

fn zero_phys_range(range: FrameRange) {
    let va = vm::phys_to_virt(range.start);
    let len = range.end - range.start;
    // SAFETY: `range` was freshly allocated from the physical frame allocator,
    // and the kernel direct map covers RAM before process creation.
    unsafe { core::ptr::write_bytes(va as *mut u8, 0, len) };
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
