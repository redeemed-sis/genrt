//! Process lifecycle, resource ownership, and userspace image operations.
//!
//! This facade is the only process API visible to other kernel subsystems.
//! Internal modules communicate through narrow sibling interfaces; the scheduler
//! does not depend on this subsystem.

mod access;
mod affinity;
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
    close_current_fd, current_cwd_dir, current_fd, current_next_fd, early_file_call,
    install_current_fd, set_current_cwd, store_file_call, take_file_call,
};
pub(crate) use affinity::process_affinity;
pub(crate) use exec::{copy_exec_args_from_user, execve_current};
pub(crate) use fork::{fork_current, set_fork_affinity_current};
pub(crate) use lifecycle::{process_exit_current, process_join};
pub(crate) use spawn::spawn_first_user_process;
pub(crate) use wait::{WaitPidAction, waitpid_current};
