use crate::{
    memory::{
        self, FrameRange, PAGE_SIZE, PhysAddr, VirtAddr,
        user::{USER_STACK_TOP, USER_TEXT_BASE},
        vm::{self, UserAddressSpace, UserMapFlags, VmError},
    },
    sched::{self, ThreadAttrs},
};

pub(crate) const USER_STACK_SIZE: usize = 64 * 1024;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessId(usize);

pub(crate) struct Process {
    id: ProcessId,
    address_space: UserAddressSpace,
    main_thread: crate::task::ThreadId,
    stack: FrameRange,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProcessError {
    Vm(VmError),
    OutOfFrames,
    Spawn(sched::SpawnError),
    Join(sched::JoinError),
}

pub(crate) fn spawn_first_user_process() -> Result<Process, ProcessError> {
    let address_space = vm::create_user_address_space().map_err(ProcessError::Vm)?;
    let mut stack = FrameRange::empty();

    let result = (|| {
        map_user_image(address_space)?;

        stack = memory::alloc_contiguous_frames(USER_STACK_SIZE / PAGE_SIZE)
            .ok_or(ProcessError::OutOfFrames)?;
        zero_phys_range(stack);
        map_user_stack(address_space, stack)?;

        let main_thread = sched::thread_spawn_user(
            address_space,
            USER_TEXT_BASE,
            USER_STACK_TOP,
            0,
            ThreadAttrs::joinable(),
        )
        .map_err(ProcessError::Spawn)?;

        Ok(Process {
            id: ProcessId(1),
            address_space,
            main_thread,
            stack,
        })
    })();

    if result.is_err() {
        if stack.start != 0 {
            memory::free_contiguous_frames(stack);
        }
        // SAFETY: the user thread was not spawned on the error paths above, so
        // no scheduler state can still reference this TTBR0 root.
        let _ = unsafe { vm::destroy_user_address_space(address_space) };
    }

    result
}

pub(crate) fn join(process: Process) -> Result<usize, ProcessError> {
    let code = sched::thread_join(process.main_thread).map_err(ProcessError::Join)?;
    crate::debug!(
        "process: joined pid={:?} main={} code={code}",
        process.id,
        process.main_thread
    );
    reclaim(process);
    Ok(code)
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
        // TTBR0 root is owned by this process until `join()` reclaims it.
        unsafe {
            vm::map_user_page(address_space, va + offset, pa + offset, flags)
                .map_err(ProcessError::Vm)?;
        }
        offset += PAGE_SIZE;
    }
    Ok(())
}

fn reclaim(process: Process) {
    memory::free_contiguous_frames(process.stack);
    // SAFETY: join has observed the only user thread exit, so no runnable or
    // blocked task can reference this address space anymore.
    if let Err(err) = unsafe { vm::destroy_user_address_space(process.address_space) } {
        crate::warn!(
            "process: failed to destroy address space for pid={:?}: {:?}",
            process.id,
            err
        );
    }
}

fn zero_phys_range(range: FrameRange) {
    let va = vm::phys_to_virt(range.start);
    let len = range.end - range.start;
    // SAFETY: `range` was freshly allocated from the physical frame allocator,
    // and the kernel direct map covers RAM before process creation.
    unsafe { core::ptr::write_bytes(va as *mut u8, 0, len) };
}
