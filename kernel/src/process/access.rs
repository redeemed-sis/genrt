use super::table::{current_process_id, table_mut};
use crate::{
    errno::{self, Errno},
    fs::{backend::Kind, call::PendingFileCall, fd::OpenDescription, vfs::Location},
};
use alloc::sync::Arc;

/// Clone current cwd under short table ownership, with no backend work.
/// # Returns
/// Pinned directory, without allocation or destruction.
/// # Errors
/// `EINVAL` if no current process/cwd exists.
pub(crate) fn current_cwd_dir() -> Result<Location, Errno> {
    let pid = current_process_id().ok_or(errno::EINVAL)?;
    table_mut()
        .slot(pid)
        .and_then(|s| s.process.resources.files.cwd.clone())
        .ok_or(errno::EINVAL)
}

/// Replace cwd by moving the old reference out before any destruction.
/// # Arguments
/// * `location` - Pinned directory selected by generic traversal.
/// # Returns
/// Success; final old reference drops after table ownership is released.
/// # Errors
/// `ENOTDIR` for files, `EINVAL` outside a process. Caller provides task window.
pub(crate) fn set_current_cwd(location: Location) -> Result<(), Errno> {
    if location.node().kind != Kind::Directory {
        return Err(errno::ENOTDIR);
    }
    let pid = current_process_id().ok_or(errno::EINVAL)?;
    let old = {
        let mut table = table_mut();
        table
            .slot_mut(pid)
            .ok_or(errno::EINVAL)?
            .process
            .resources
            .files
            .cwd
            .replace(location)
    };
    drop(old);
    Ok(())
}

/// Check descriptor capacity before backend create/open/truncate work.
/// # Returns
/// Lowest free slot, stable across waits in the one-thread-per-process model.
/// # Errors
/// `EBADF` outside a process, `EMFILE` when full; allocation-free.
pub(crate) fn current_next_fd() -> Result<usize, Errno> {
    let pid = current_process_id().ok_or(errno::EBADF)?;
    table_mut()
        .slot(pid)
        .ok_or(errno::EBADF)?
        .process
        .resources
        .files
        .fds
        .next_free()
}

/// Install a staged OFD by bounded ownership transfer only.
/// # Arguments
/// * `description` - Taken only on success; caller drops failures outside locks.
/// # Returns
/// Lowest free descriptor, without callbacks or allocation.
/// # Errors
/// `EBADF` outside process, `EMFILE` for capacity.
pub(crate) fn install_current_fd(
    description: &mut Option<Arc<OpenDescription>>,
) -> Result<usize, Errno> {
    let pid = current_process_id().ok_or(errno::EBADF)?;
    table_mut()
        .slot_mut(pid)
        .ok_or(errno::EBADF)?
        .process
        .resources
        .files
        .fds
        .install(description)
}

/// Clone an OFD without invoking a backend or allocating.
/// # Arguments
/// * `fd` - Current process ordinary descriptor.
/// # Returns
/// Shared OFD reference; caller owns its destruction in task context.
/// # Errors
/// `EBADF` outside process or for invalid descriptors.
pub(crate) fn current_fd(fd: usize) -> Result<Arc<OpenDescription>, Errno> {
    let pid = current_process_id().ok_or(errno::EBADF)?;
    table_mut()
        .slot(pid)
        .ok_or(errno::EBADF)?
        .process
        .resources
        .files
        .fds
        .get(fd)
}

/// Remove an FD and release its reference after the process lock is gone.
/// # Arguments
/// * `fd` - Ordinary descriptor to close.
/// # Returns
/// Success after finite backend release in the caller's task window.
/// # Errors
/// `EBADF` outside process or for invalid descriptors.
pub(crate) fn close_current_fd(fd: usize) -> Result<(), Errno> {
    let pid = current_process_id().ok_or(errno::EBADF)?;
    let removed = {
        table_mut()
            .slot_mut(pid)
            .ok_or(errno::EBADF)?
            .process
            .resources
            .files
            .fds
            .take(fd)?
    };
    drop(removed);
    Ok(())
}

/// Extract retained syscall ownership for resume outside the table lock.
/// # Returns
/// Pending call or none; allocation-free and no destructor work.
pub(crate) fn take_file_call() -> Option<PendingFileCall> {
    let pid = current_process_id()?;
    table_mut()
        .slot_mut(pid)?
        .process
        .resources
        .files
        .pending
        .take()
}

/// Publish owned continuation before committing its exact scheduler wait.
/// # Arguments
/// * `call` - Fully owned call; no borrowed stack/frame/user data.
/// # Returns
/// Nothing; bounded move under short table ownership.
/// # Panics
/// Missing process or duplicate continuation violates single-thread ownership.
pub(crate) fn store_file_call(call: PendingFileCall) {
    let pid = current_process_id().expect("vfs: pending call without process");
    let mut table = table_mut();
    let pending = &mut table.slot_mut(pid).unwrap().process.resources.files.pending;
    assert!(pending.is_none());
    *pending = Some(call);
}

/// Clear an early-consumed wait identity without dropping its continuation.
/// # Returns
/// Nothing; bounded mutation, no allocation or backend work.
pub(crate) fn early_file_call() {
    let pid = current_process_id().unwrap();
    table_mut()
        .slot_mut(pid)
        .unwrap()
        .process
        .resources
        .files
        .pending
        .as_mut()
        .unwrap()
        .waited = None;
}
