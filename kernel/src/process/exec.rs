use crate::{
    arch::ActiveContext,
    errno,
    fs::{path, ramfs},
    loader::elf::{self, UserElfImage},
    memory::{
        user::OwnedUserStack,
        vm::{self, OwnedUserAddressSpace},
    },
    sync::LocalIrqGuard,
};

use super::{
    USER_STACK_SIZE,
    error::{exec_elf_errno, exec_vm_errno},
    id::ProcessId,
    image::{ExecArgs, build_initial_user_stack, copy_exec_args_from_user as copy_args},
    resources::{ProcessImageResources, cleanup_process_image_resources},
    table::{current_process_id, table_mut},
};

struct StagedProcessImage {
    address_space: OwnedUserAddressSpace,
    user_image: UserElfImage,
    stack: OwnedUserStack,
    entry: usize,
    initial_sp: usize,
}

pub(crate) fn copy_exec_args_from_user(
    path: &[u8],
    argv_ptr: usize,
    envp_ptr: usize,
) -> Result<ExecArgs, errno::Errno> {
    copy_args(path, argv_ptr, envp_ptr)
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
    cleanup_process_image_resources(pid, old);
    Ok(())
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
        // SAFETY: staged exec was never published, so no thread can reference this root.
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
) -> (ProcessImageResources, crate::sched::UserThreadResources) {
    let entry = staged.entry;
    let initial_sp = staged.initial_sp;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    if table_mut().slot_mut(pid).is_none() {
        panic!("process: validated exec slot disappeared before commit");
    }
    let old_stack =
        crate::sched::replace_current_user_resources(staged.address_space.id(), staged.stack)
            .unwrap_or_else(|_| panic!("process: staged exec address-space activation failed"));
    let slot = table_mut()
        .slot_mut(pid)
        .unwrap_or_else(|| panic!("process: prevalidated exec slot disappeared during swap"));
    let old = ProcessImageResources {
        address_space: slot
            .process
            .resources
            .image
            .address_space
            .replace(staged.address_space),
        user_image: slot
            .process
            .resources
            .image
            .user_image
            .replace(staged.user_image),
    };
    slot.process.state = super::state::ProcessState::Running;
    slot.process.exit_status = None;
    context.replace_user_context_after_exec(entry, initial_sp);
    (old, old_stack)
}
