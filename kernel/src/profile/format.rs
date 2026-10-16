//! Fixed V1 export layout, configuration validation, and initialized-byte views.

use crate::errno::{self, Errno};

pub(super) const BUDGET: usize = 4 * 1024 * 1024;
pub(super) const PHASE_COUNT: usize = 14;
pub(super) const MAGIC: u32 = 0x31505747; // GWP1 in little-endian bytes.
pub(super) const FULL: u32 = 1;
pub(super) const FOCUSED: u32 = 2;
pub(super) const IDLE: u32 = 1;
pub(super) const GENERIC: u32 = 2;
pub(super) const EARLY: u32 = 3;

/// V1 probe identifiers; discriminants index the exported counter array.
#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub(super) enum Phase {
    /// First assembly probe after x0/x1 preservation.
    SvcEntry = 0,
    /// Absolute timespec conversion and validation completed.
    DeadlineValidated,
    /// Exact wait inserted and nearest timer programmed.
    TimerPublished,
    /// Scheduler committed blocking state.
    WaitCommitted,
    /// Sleep context/address-space handoff completed.
    SleepHandoff,
    /// First assembly IRQ probe, attached at exact expiration.
    IrqEntry,
    /// Platform interrupt acknowledgement completed.
    Acknowledge,
    /// Exact deadline extracted from the time queue.
    ExpiredExtracted,
    /// Timer dispatch acquired scheduler ownership.
    SchedulerAcquired,
    /// Owner committed as Running/current, before validation and context handoff.
    WakeSelected,
    /// Saved context restored and address space activated.
    ContextRestored,
    /// Originating IRQ completed timer rearm bookkeeping.
    TimerRearmed,
    /// Originating IRQ completed controller end operation.
    InterruptCompleted,
    /// Final assembly return hook before restoring GPRs, not eret.
    ReturnToUser,
}

/// Four fixed-width configuration fields from userspace; no pointer fields.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    /// Full=1 or focused=2.
    pub(crate) mode: u32,
    /// Maximum retained sleep cycles, not phase count.
    pub(crate) max_records: u32,
    /// Selected focused start probe; validated even in full mode.
    pub(crate) start_phase: u32,
    /// Selected focused end probe, strictly greater than start.
    pub(crate) end_phase: u32,
}

/// Fixed session metadata; integer-only fields and assertions forbid padding.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct Header {
    pub(super) magic: u32,
    pub(super) version: u32,
    pub(super) header_bytes: u32,
    pub(super) record_bytes: u32,
    pub(super) source_version: u32,
    pub(super) cpu: u32,
    pub(super) owner_index: u32,
    pub(super) owner_generation: u32,
    pub(super) frequency: u64,
    pub(super) mode: u32,
    pub(super) start_phase: u32,
    pub(super) end_phase: u32,
    pub(super) capacity: u32,
    pub(super) recorded: u64,
    pub(super) incomplete: u64,
    pub(super) lost: u64,
    pub(super) early: u64,
    pub(super) counter_pair_ticks: u64,
    pub(super) trace_write_ticks: u64,
}

/// One cycle's identity, presence mask, and raw counters in the V1 export.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct Record {
    pub(super) ordinal: u64,
    pub(super) thread_generation: u32,
    pub(super) wake_path: u32,
    pub(super) wait_sequence: u64,
    pub(super) requested_deadline: u64,
    pub(super) programmed_deadline: u64,
    pub(super) phase_present: u64,
    pub(super) counters: [u64; PHASE_COUNT],
}

const _: () = {
    assert!(core::mem::size_of::<Config>() == 16);
    assert!(core::mem::size_of::<Header>() == 104);
    assert!(core::mem::offset_of!(Header, frequency) == 32);
    assert!(core::mem::offset_of!(Header, recorded) == 56);
    assert!(core::mem::offset_of!(Header, counter_pair_ticks) == 88);
    assert!(core::mem::size_of::<Record>() == 160);
    assert!(core::mem::offset_of!(Record, wait_sequence) == 16);
    assert!(core::mem::offset_of!(Record, counters) == 48);
    assert!(cfg!(target_endian = "little"));
};

impl Config {
    /// Validate settings and calculate checked session bytes, without allocation.
    /// # Returns
    /// Header plus configured record bytes, within the per-session budget.
    /// # Errors
    /// EINVAL for invalid mode, capacity, pair or budget; EOVERFLOW for size overflow.
    pub(super) fn bytes(self) -> Result<usize, Errno> {
        if !matches!(self.mode, FULL | FOCUSED)
            || self.max_records == 0
            || self.start_phase >= self.end_phase
            || self.end_phase as usize >= PHASE_COUNT
        {
            return Err(errno::EINVAL);
        }
        let bytes = (self.max_records as usize)
            .checked_mul(core::mem::size_of::<Record>())
            .and_then(|n| n.checked_add(core::mem::size_of::<Header>()))
            .ok_or(errno::EOVERFLOW)?;
        if bytes > BUDGET {
            return Err(errno::EINVAL);
        }
        Ok(bytes)
    }
}

// Both export types consist exclusively of initialized fixed-width integers,
// with asserted layouts and no padding; no other type is passed to this helper.
/// Borrow an initialized V1 layout without copying or allocation.
/// # Arguments
/// * `value` - Header or Record, with an asserted integer-only, padding-free layout.
/// # Returns
/// The initialized representation, borrowed for the lifetime of `value`.
pub(super) fn bytes<T>(value: &T) -> &[u8] {
    // SAFETY: callers use only Header/Record with all bytes initialized above;
    // the borrow cannot outlive value and there is no concurrent mutation.
    unsafe {
        core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>())
    }
}
