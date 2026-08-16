//! Architecture-neutral kernel runtime configuration.

/// Maximum pathname length in bytes, excluding the terminating NUL.
///
/// The limit bounds userspace pathname scans and canonical path construction.
/// It is part of the current genrt userspace ABI.
pub const GENRT_PATH_MAX: usize = 4096;

/// Round-robin scheduler quantum in milliseconds.
pub(crate) const SCHED_RR_QUANTUM_MS: u64 = 10;

/// Total preallocated kernel thread slots, including idle and bootstrap threads.
pub(crate) const KERNEL_THREAD_CAPACITY: usize = 12;

/// Maximum logical CPUs represented by fixed kernel execution-local storage.
///
/// CPU0 may register secondary CPUs up to this capacity. Secondary scheduler
/// contexts remain offline until a later SMP milestone explicitly activates
/// them.
pub(crate) const KERNEL_CPU_CAPACITY: usize = 4;
