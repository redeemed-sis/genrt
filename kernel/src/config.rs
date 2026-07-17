//! Architecture-neutral kernel runtime configuration.

/// Round-robin scheduler quantum in milliseconds.
pub(crate) const SCHED_RR_QUANTUM_MS: u64 = 10;

/// Total preallocated kernel thread slots, including idle and bootstrap threads.
pub(crate) const KERNEL_THREAD_CAPACITY: usize = 12;
