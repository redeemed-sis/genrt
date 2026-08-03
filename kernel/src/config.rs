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
/// The active implementation registers and runs only CPU0. The remaining
/// slots are deliberately present for later SMP milestones and do not imply
/// secondary boot, interrupts, or scheduler execution.
pub(crate) const KERNEL_CPU_CAPACITY: usize = 4;
