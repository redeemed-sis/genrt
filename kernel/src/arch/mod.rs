//! Architecture-facing live-context and syscall boundary.

mod context;

pub use context::{ActiveContext, SavedContext, SyscallRequest};
