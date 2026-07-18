use crate::{
    fs::{fd::FdTable, initramfs, ramfs},
    loader::elf::{self, UserElfImage},
    memory::{
        user::OwnedUserStack,
        vm::{self, OwnedUserAddressSpace},
    },
    sched::{self, ThreadAttrs},
    sync::LocalIrqGuard,
};

use super::{
    USER_STACK_SIZE,
    error::ProcessError,
    id::ProcessId,
    image::{ExecArgs, build_initial_user_stack},
    table::{
        allocate_process_slot, attach_process_main_thread, attach_process_resources,
        free_process_slot, table_mut, take_process_image_resources,
    },
};

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
            .push(super::image::ExecStringVector::Argv, b"/init".to_vec())
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
                    .process
                    .resources
                    .image
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
        let attached = take_process_image_resources(pid);
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
        // SAFETY: a failed spawn path never leaves a runnable user thread that can reference this root.
        let _ = unsafe { vm::destroy_user_address_space(address_space) };
        free_process_slot(pid);
    }
    result
}

fn load_init_image(
    address_space: &mut OwnedUserAddressSpace,
) -> Result<UserElfImage, ProcessError> {
    let image = initramfs::init_file().map_err(ProcessError::Initramfs)?;
    crate::info!("init: loading /init from initramfs");
    crate::info!("init: /init size={}", image.len());
    elf::load_user_elf(image, address_space).map_err(ProcessError::Elf)
}
