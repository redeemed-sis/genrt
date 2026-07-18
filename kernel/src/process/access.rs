use crate::{
    fs::{
        fd::{FdDirEntry, FdError},
        ramfs,
    },
    sync::LocalIrqGuard,
};

use super::{
    error::ProcessPathError,
    files::ProcessFileState,
    table::{current_process_id, table_mut},
};

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
    table_mut()
        .slot_mut(pid)
        .ok_or(ProcessPathError::NoCurrentProcess)?
        .process
        .resources
        .files
        .cwd_dir()
        .ok_or(ProcessPathError::InvalidDirectory)
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
    ramfs::dir_path(current_cwd_dir()?).ok_or(ProcessPathError::InvalidDirectory)
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
    table_mut()
        .slot_mut(pid)
        .ok_or(ProcessPathError::NoCurrentProcess)?
        .process
        .resources
        .files
        .set_cwd(dir_index);
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
    with_current_file_state_mut(|files| files.open_ram_file(file_index))
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
    with_current_file_state_mut(|files| files.open_ram_dir(dir_index))
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
    with_current_file_state(|files| files.read_slice(fd, max_len))
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
    with_current_file_state_mut(|files| files.advance_read(fd, amount))
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
) -> Result<Option<FdDirEntry>, FdError> {
    with_current_file_state(|files| files.dir_entry_at(fd, relative_offset))
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
    with_current_file_state_mut(|files| files.advance_dir_read(fd, amount))
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
    with_current_file_state_mut(|files| files.close(fd))
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
    with_current_file_state(|files| Ok(files.is_open(fd)))
}

fn with_current_file_state<T>(
    f: impl FnOnce(&ProcessFileState) -> Result<T, FdError>,
) -> Result<T, FdError> {
    let pid = current_process_id().ok_or(FdError::BadFd)?;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let process = &table_mut().slot_mut(pid).ok_or(FdError::BadFd)?.process;
    f(&process.resources.files)
}

fn with_current_file_state_mut<T>(
    f: impl FnOnce(&mut ProcessFileState) -> Result<T, FdError>,
) -> Result<T, FdError> {
    let pid = current_process_id().ok_or(FdError::BadFd)?;
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let process = &mut table_mut().slot_mut(pid).ok_or(FdError::BadFd)?.process;
    f(&mut process.resources.files)
}
