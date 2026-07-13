//! Test-only QEMU coordinators and machine-readable protocol support.

pub(crate) mod protocol;

#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) mod kernel_runtime;

#[cfg(feature = "qemu-test-user-fault")]
pub(crate) mod user_fault;
