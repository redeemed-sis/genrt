use alloc::vec::Vec;

use crate::{
    arch::ActiveContext,
    errno,
    loader::elf::{self, UserElfImage, UserElfSegment},
    memory::{
        self,
        user::OwnedUserStack,
        vm::{self, StagedUserAddressSpace},
    },
    sched::{self, ThreadAttrs},
    sync::LocalIrqGuard,
};

use super::{
    error::{fork_errno, fork_vm_errno, spawn_errno},
    id::ProcessId,
    table::{
        allocate_process_slot, attach_process_resources, bind_published_user_thread,
        consume_pending_fork_cpu, current_process_id, free_process_slot,
        set_current_pending_fork_cpu, table_mut, take_process_image_resources,
    },
};

/// Set or reset the current process's one-shot child CPU placement.
///
/// # Arguments
///
/// * `raw_cpu` - Nonnegative registered, online logical CPU index, or `-1` to
///   reset the next-fork override.
///
/// # Returns
///
/// Returns `Ok(())` after replacing the pending policy. The next successfully
/// published child consumes it; failed pre-publication fork staging leaves it
/// unchanged. This operation takes only short process/scheduler ownership and
/// does not allocate or block.
///
/// # Errors
///
/// Returns `EINVAL` for an invalid negative value, unregistered/offline CPU,
/// or when no current process owns the request.
pub(crate) fn set_fork_affinity_current(raw_cpu: isize) -> Result<(), errno::Errno> {
    let pending = match raw_cpu {
        -1 => None,
        value if value < -1 => return Err(errno::EINVAL),
        value => {
            let cpu = crate::cpu::CpuId::from_index(value as usize).ok_or(errno::EINVAL)?;
            if !crate::cpu::is_registered(cpu) || !sched::scheduler_online(cpu) {
                return Err(errno::EINVAL);
            }
            Some(cpu)
        }
    };
    set_current_pending_fork_cpu(pending)
        .then_some(())
        .ok_or(errno::EINVAL)
}

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
/// that would violate process-table lock ownership and slot reservation.
pub(crate) fn fork_current(context: &mut ActiveContext<'_>) -> Result<usize, errno::Errno> {
    let snapshot = fork_snapshot_current()?;
    let child_pid = allocate_process_slot(
        Some(snapshot.parent_pid),
        snapshot.fds,
        snapshot.cwd_dir,
        snapshot.child_owner_cpu,
    )
    .map_err(fork_errno)?;
    let mut staged_address_space = None;
    let mut user_image = None;
    let mut stack = None;
    let result = (|| {
        staged_address_space = Some(vm::create_user_address_space().map_err(fork_vm_errno)?);
        let child_aspace = staged_address_space.as_ref().ok_or(errno::ENOMEM)?;
        user_image = Some(clone_user_image(snapshot.user_image(), child_aspace)?);
        // SAFETY: the current parent thread cannot be reaped while it synchronously clones its stack.
        stack =
            Some(unsafe { (&*snapshot.stack).clone_into(child_aspace) }.map_err(fork_vm_errno)?);
        attach_process_resources(
            child_pid,
            staged_address_space
                .take()
                .ok_or(errno::ENOMEM)?
                .assign_owner(snapshot.child_owner_cpu),
            user_image.take().ok_or(errno::ENOMEM)?,
        );
        {
            let _irq_guard = LocalIrqGuard::save_and_disable();
            let mut table = table_mut();
            let child_aspace_id = table
                .slot(child_pid)
                .ok_or(errno::ENOMEM)?
                .process
                .resources
                .image
                .address_space
                .as_ref()
                .ok_or(errno::ENOMEM)?
                .id();
            let child_thread = sched::thread_spawn_user_from_context(
                child_aspace_id,
                stack.take().ok_or(errno::ENOMEM)?,
                context,
                ThreadAttrs::joinable().with_affinity(snapshot.child_owner_cpu),
                |thread, home_cpu, address_space| {
                    bind_published_user_thread(
                        &mut table,
                        child_pid,
                        thread,
                        home_cpu,
                        address_space,
                    );
                    consume_pending_fork_cpu(&mut table, snapshot.parent_pid);
                },
            )
            .map_err(|(error, returned_stack)| {
                stack = Some(returned_stack);
                spawn_errno(error)
            })?;
            let _ = child_thread;
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
        if let Some(aspace) = attached.address_space {
            // SAFETY: failed fork did not publish a runnable child using this root.
            let _ = unsafe { vm::destroy_user_address_space(aspace) };
        } else if let Some(aspace) = staged_address_space {
            let _ = vm::destroy_staged_user_address_space(aspace);
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
    child_owner_cpu: crate::cpu::CpuId,
}

impl ForkSnapshot {
    fn user_image(&self) -> &UserElfImage {
        // SAFETY: current one-thread-per-process execution cannot reclaim its own image during synchronous fork.
        unsafe { &*self.user_image }
    }
}

fn fork_snapshot_current() -> Result<ForkSnapshot, errno::Errno> {
    let parent_pid = current_process_id().ok_or(errno::EINVAL)?;
    // SAFETY: synchronous fork cannot reap its currently running parent before
    // clone consumes this pointer.
    let stack = unsafe { sched::current_user_stack_ptr() }.ok_or(errno::EINVAL)?;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let mut table = table_mut();
    let slot = table.slot_mut(parent_pid).ok_or(errno::EINVAL)?;
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
        stack,
        fds,
        cwd_dir,
        child_owner_cpu: slot.process.next_child_cpu(),
    })
}

fn clone_user_image(
    src: &UserElfImage,
    dst_aspace: &StagedUserAddressSpace,
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
    dst_aspace: &StagedUserAddressSpace,
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
