//! Bounded, CPU-local diagnostic sleep tracing. The profiler lock is a leaf:
//! callers capture thread identity before entry; no scheduler, usercopy, heap,
//! or callback executes while it is held. Storage is allocated/dropped only in
//! task context outside owner locks. A session has a 4 MiB allocation budget.
//! Export is little-endian V1, matching the public `genrt/profile.h` schema.
//! Entry probes are after preserving x0/x1; return is before GPR restoration,
//! not the hardware exception or actual eret boundary. Focused mode suppresses
//! unselected Rust counter reads, but not the fixed assembly entry probe.

// Keep callers independent of the private representation and runtime storage.
mod format;
mod runtime;
mod session;

pub(crate) use format::Config;
pub(crate) use runtime::{
    WakePath, begin, bind, cleanup, context_restored, control, expired, read, scheduler_acquired,
    sleep_committed, sleep_handoff, timer_rearmed, wait_published, wake_selected,
};
pub use runtime::{interrupt_completed, irq_ack, irq_entry, programmed, return_to_user, svc_entry};

unsafe extern "C" {
    fn arch_profile_counter() -> u64;
}

fn counter() -> u64 {
    // SAFETY: architecture hook reads the shared physical counter with ISB.
    unsafe { arch_profile_counter() }
}
