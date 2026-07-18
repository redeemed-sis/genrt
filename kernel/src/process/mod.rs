//! Process lifecycle, resource ownership, and userspace image operations.
//!
//! This facade is the only process API visible to other kernel subsystems.
//! Internal modules communicate through narrow sibling interfaces; the scheduler
//! does not depend on this subsystem.

mod access;
mod error;
mod exec;
mod fault;
mod files;
mod fork;
mod id;
mod image;
mod lifecycle;
mod record;
mod resources;
mod spawn;
mod state;
mod table;
mod wait;

/// Size of each thread-owned userspace stack mapping.
///
/// The value is used only while constructing a user thread in normal thread
/// context; it neither allocates nor changes scheduler state.
pub(crate) const USER_STACK_SIZE: usize = 64 * 1024;

pub use error::ProcessFaultError;
pub use fault::kill_current_process_on_user_fault;
pub use state::{ProcessExitStatus, UserFault, UserFaultKind};

pub(crate) use access::{
    advance_current_fd_dir_read, advance_current_fd_read, close_current_fd, current_cwd_dir,
    current_cwd_path, current_fd_dir_entry_at, current_fd_is_open, current_fd_read_slice,
    open_current_ram_dir, open_current_ram_file, set_current_cwd,
};
pub(crate) use error::ProcessPathError;
pub(crate) use exec::{copy_exec_args_from_user, execve_current};
pub(crate) use fork::fork_current;
pub(crate) use lifecycle::{process_exit_current, process_join};
pub(crate) use spawn::spawn_first_user_process;
pub(crate) use wait::{WaitPidAction, waitpid_current};
