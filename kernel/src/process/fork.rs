use alloc::vec::Vec;

use crate::{
    arch::ActiveContext,
    errno,
    loader::elf::{self, UserElfImage, UserElfSegment},
    memory::{
        self,
        user::OwnedUserStack,
        vm::{self, OwnedUserAddressSpace},
    },
    sched::{self, ThreadAttrs},
    sync::LocalIrqGuard,
};

use super::{
    error::{fork_errno, fork_vm_errno, spawn_errno},
    id::ProcessId,
    table::{
        allocate_process_slot, attach_process_main_thread, attach_process_resources,
        current_process_id, free_process_slot, table_mut, take_process_image_resources,
    },
};

/// Eagerly clone the current process and live userspace resume context.
///
/// This thread-context operation may allocate and copy process-owned memory.
/// Only final child publication and the scheduler-frame clone run in a short
/// local IRQ-disabled section; scheduler storage remains preallocated.
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
/// Panics if a reserved child process slot disappears after publication begins;
/// that would violate single-core process-table ownership.
pub(crate) fn fork_current(context: &mut ActiveContext<'_>) -> Result<usize, errno::Errno> {
    let snapshot = fork_snapshot_current()?;
    let child_pid =
        allocate_process_slot(Some(snapshot.parent_pid), snapshot.fds, snapshot.cwd_dir)
            .map_err(fork_errno)?;
    let mut address_space = None;
    let mut user_image = None;
    let mut stack = None;
    let result = (|| {
        address_space = Some(vm::create_user_address_space().map_err(fork_vm_errno)?);
        let child_aspace = address_space.as_ref().ok_or(errno::ENOMEM)?;
        user_image = Some(clone_user_image(snapshot.user_image(), child_aspace)?);
        // SAFETY: the current parent thread cannot be reaped while it synchronously clones its stack.
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
        let attached = take_process_image_resources(child_pid);
        if let Some(image) = user_image {
            elf::free_loaded_segments(&image);
        } else if let Some(image) = attached.user_image {
            elf::free_loaded_segments(&image);
        }
        drop(stack);
        if let Some(aspace) = attached.address_space.or(address_space) {
            // SAFETY: failed fork did not publish a runnable child using this root.
            let _ = unsafe { vm::destroy_user_address_space(aspace) };
        }
        free_process_slot(child_pid);
    }
    result
}

struct ForkSnapshot {
    parent_pid: ProcessId,
    user_image: *const UserElfImage,
    stack: *const OwnedUserStack,
    fds: crate::fs::fd::FdTable,
    cwd_dir: usize,
}

impl ForkSnapshot {
    fn user_image(&self) -> &UserElfImage {
        // SAFETY: current one-thread-per-process execution cannot reclaim its own image during synchronous fork.
        unsafe { &*self.user_image }
    }
}

fn fork_snapshot_current() -> Result<ForkSnapshot, errno::Errno> {
    let parent_pid = current_process_id().ok_or(errno::EINVAL)?;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let slot = table_mut().slot_mut(parent_pid).ok_or(errno::EINVAL)?;
    let user_image = slot
        .process
        .resources
        .image
        .user_image
        .as_ref()
        .ok_or(errno::EINVAL)? as *const UserElfImage;
    let (fds, cwd_dir) = slot.process.resources.files.snapshot();
    let cwd_dir = cwd_dir.ok_or(errno::EINVAL)?;
    Ok(ForkSnapshot {
        parent_pid,
        user_image,
        // SAFETY: synchronous fork cannot reap its currently running parent before clone consumes this pointer.
        stack: unsafe { sched::current_user_stack_ptr() }.ok_or(errno::EINVAL)?,
        fds,
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
        match clone_user_segment(*segment, dst_aspace) {
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
    let frames = memory::clone_frame_range(segment.frames).map_err(|err| match err {
        memory::FrameRangeCloneError::InvalidRange => errno::EINVAL,
        memory::FrameRangeCloneError::OutOfFrames => errno::ENOMEM,
    })?;
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

fn free_user_segments(segments: &[UserElfSegment]) {
    for segment in segments {
        if segment.frames.start != 0 {
            memory::free_contiguous_frames(segment.frames);
        }
    }
}
