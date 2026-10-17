//! Per-CPU leaf ownership, task-context control, and scheduler/IRQ hooks.

use super::{
    counter,
    format::{Config, FOCUSED, Phase, Record},
    session::Session,
};
use crate::{
    cpu::{self, CpuId},
    errno::{self, Errno},
    sched::{self, ThreadId, WaitToken},
    sync::{IrqSpinLock, preempt::PreemptGuard},
};

/// Scheduler selection path; early syscall return is classified at admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WakePath {
    /// Owner selected directly from idle by its exact timed wake.
    Idle,
    /// Shared scheduler handoff; does not erase an earlier Idle classification.
    Generic,
}

#[used]
static DIAGNOSTIC_MARKER: [u8; 21] = *b"GENRT_WAKE_PROFILE_V1";

struct CpuProfile {
    session: Option<Session>,
    svc: Option<(ThreadId, u64)>,
    irq_entry: u64,
    ack: Option<u64>,
    irq_record: Option<usize>,
}
impl CpuProfile {
    const fn new() -> Self {
        Self {
            session: None,
            svc: None,
            irq_entry: 0,
            ack: None,
            irq_record: None,
        }
    }
    fn start_allowed(&self, owner: ThreadId) -> Result<(), Errno> {
        match &self.session {
            Some(s) if s.owner != owner || s.active => Err(errno::EBUSY),
            _ => Ok(()),
        }
    }
    fn take_owned(&mut self, owner: ThreadId) -> Result<Session, Errno> {
        let session = self.session.as_ref().ok_or(errno::ENOENT)?;
        if session.owner != owner {
            return Err(errno::EPERM);
        }
        self.irq_record = None;
        Ok(self.session.take().expect("profile: checked session"))
    }
    fn detach_owner(&mut self, owner: ThreadId) -> Option<Session> {
        if self.session.as_ref().is_some_and(|s| s.owner == owner) {
            self.svc = None;
            self.irq_record = None;
            self.session.take()
        } else {
            None
        }
    }

    fn begin(&mut self, deadline: u64, early: bool) {
        if let Some((owner, entry)) = self.svc.take() {
            if let Some(session) = self.session.as_mut() {
                session.begin(owner, deadline, early, entry);
            }
        }
    }
}
static CPU: [IrqSpinLock<CpuProfile>; crate::config::KERNEL_CPU_CAPACITY] =
    [const { IrqSpinLock::new(CpuProfile::new()) }; crate::config::KERNEL_CPU_CAPACITY];

unsafe extern "C" {
    fn arch_profile_task_irq_enable() -> u64;
    fn arch_local_irq_restore(saved_daif: u64);
}

// Syscall/fault entries begin IRQ-masked. Heap work must admit IRQs without
// allowing a scheduler handoff on a borrowed live syscall frame. Restore the
// exact entry state before dropping preemption exclusion or returning to caller.
struct TaskHeapWindow {
    saved: u64,
    _preempt: PreemptGuard,
}
impl TaskHeapWindow {
    fn enter() -> Self {
        let preempt = PreemptGuard::enter();
        // SAFETY: only control/cleanup task-context callers, with no shared
        // locks; PreemptGuard prevents IRQ-return selection while unmasked.
        let saved = unsafe { arch_profile_task_irq_enable() };
        Self {
            saved,
            _preempt: preempt,
        }
    }
}
impl Drop for TaskHeapWindow {
    fn drop(&mut self) {
        // SAFETY: paired state on the same CPU (non-send PreemptGuard).
        // Field destruction releases preemption only after IRQ restoration.
        unsafe {
            arch_local_irq_restore(self.saved);
        }
    }
}
fn cpu() -> CpuId {
    cpu::current_id().expect("profile: unbound CPU")
}
fn slot(cpu: CpuId) -> &'static IrqSpinLock<CpuProfile> {
    &CPU[cpu.index()]
}

/// Control one immutable-CPU session in normal syscall task context.
/// Allocation, calibration and old storage destruction occur outside locks.
/// # Arguments
/// * `owner` - Exact current process main-thread identity, captured beforehand.
/// * `command` - START=1, STOP=2, DISCARD=3.
/// * `config` - Required only for START; validated fixed-width settings.
/// # Returns
/// Returns zero after publishing, freezing or discarding the session.
/// # Errors
/// EINVAL for invalid configuration/command, ENOMEM for allocation failure,
/// EBUSY for active START/foreign session, EPERM for foreign STOP/DISCARD,
/// ENOENT when stopping absent storage, EOVERFLOW for size arithmetic overflow.
pub(crate) fn control(
    owner: ThreadId,
    command: usize,
    config: Option<Config>,
) -> Result<usize, Errno> {
    core::hint::black_box(&DIAGNOSTIC_MARKER);
    let cpu = cpu();
    let _heap_window = TaskHeapWindow::enter();
    match command {
        1 => {
            slot(cpu).lock().start_allowed(owner)?;
            let config = config.ok_or(errno::EINVAL)?;
            let mut new = Session::allocate(owner, cpu, config, |v, count| {
                v.try_reserve_exact(count).map_err(|_| errno::ENOMEM)
            })?;
            // Probe baseline: ordered read pair, then one selected trace write
            // bracketed by ordered reads. No active session is published yet.
            let a = counter();
            let b = counter();
            new.header.counter_pair_ticks = b.saturating_sub(a);
            let a = counter();
            // Calibrate a real write even if ReturnToUser is not selected.
            let phase = if config.mode == FOCUSED {
                config.start_phase as usize
            } else {
                13
            };
            new.records[0].counters[phase] = a;
            new.records[0].phase_present = 1 << phase;
            core::hint::black_box(&new.records[0]);
            let b = counter();
            new.header.trace_write_ticks = b.saturating_sub(a);
            new.records[0] = Record::default();
            let old = {
                let mut state = slot(cpu).lock();
                state.start_allowed(owner)?;
                state.svc = None;
                state.irq_record = None;
                state.session.replace(new)
            };
            drop(old);
            Ok(0)
        }
        2 | 3 => {
            let mut old = {
                let mut state = slot(cpu).lock();
                state.take_owned(owner)?
            };
            if command == 2 {
                // Constant-time freeze; validity accounted at the final probe.
                old.freeze();
                slot(cpu).lock().session = Some(old);
            } else {
                drop(old);
            }
            Ok(0)
        }
        _ => Err(errno::EINVAL),
    }
}

/// Copy a frozen export chunk into kernel staging without heap snapshots.
/// Uses only a leaf lock; usercopy must occur after this function returns.
/// # Arguments
/// * `owner` - Captured current process main-thread identity.
/// * `offset` - Byte offset; at/beyond EOF returns zero, including usize::MAX.
/// * `output` - Kernel staging slice bounded by MAX_USER_COPY.
/// # Returns
/// Number of copied bytes, or zero at EOF.
/// # Errors
/// EINVAL for oversized staging, ENOENT for absent session, EPERM for foreign
/// ownership, EBUSY while recording.
pub(crate) fn read(owner: ThreadId, offset: usize, output: &mut [u8]) -> Result<usize, Errno> {
    if output.len() > crate::memory::user::MAX_USER_COPY {
        return Err(errno::EINVAL);
    }
    let state = slot(cpu()).lock();
    state
        .session
        .as_ref()
        .ok_or(errno::ENOENT)?
        .read(owner, offset, output)
}

/// Detach own session before normal/fault process termination acquires locks.
/// Must run in task context; heap destruction is after profiler exclusion.
/// # Arguments
/// * `owner` - Exact terminating process main thread; foreign storage is kept.
/// # Returns
/// Returns after owned storage is freed, or unchanged when absent/foreign.
pub(crate) fn cleanup(owner: ThreadId) {
    let _heap_window = TaskHeapWindow::enter();
    let old = {
        let mut state = slot(cpu()).lock();
        state.detach_owner(owner)
    };
    drop(old);
}

/// Stage a live lower-EL SVC assembly entry probe. No allocation or scheduler
/// call occurs under the leaf lock.
/// # Arguments
/// * `entry` - Raw probe from live exception padding, never SavedContext.
/// # Returns
/// Returns after staging the current exact owner, or clearing stale staging
/// when no current thread exists.
pub fn svc_entry(entry: u64) {
    let owner = sched::current_thread_id();
    slot(cpu()).lock().svc = owner.map(|owner| (owner, entry));
}

/// Begin one validated absolute sleep cycle in preallocated storage.
/// Consumes the current SVC's staged owner and entry exactly once. Syscall
/// admission has not handed off or admitted IRQs since staging; no second
/// scheduler lookup is needed. The session still checks the exact owner.
/// # Arguments
/// * `deadline` - Checked absolute counter deadline.
/// * `early` - Deadline already passed, so no blocked wait is claimed.
/// # Returns
/// Returns after retention or loss accounting; foreign/inactive calls ignored.
/// # Panics
/// Panics if the executing CPU has no valid logical binding.
pub(crate) fn begin(deadline: u64, early: bool) {
    slot(cpu()).lock().begin(deadline, early);
}

/// Associate the prepared exact wait with the currently validated cycle.
/// # Arguments
/// * `token` - CPU/generation/sequence checked scheduler preparation.
/// * `deadline` - Original absolute deadline, required to match the cycle.
/// # Returns
/// Returns after binding, or unchanged for unrelated calls. Allocation-free.
pub(crate) fn bind(token: WaitToken, deadline: u64) {
    let mut state = slot(token.cpu()).lock();
    if let Some(s) = state.session.as_mut() {
        s.bind(token, deadline);
    }
}

/// Observe timer publication for the exact bound wait under the leaf lock.
/// Unselected probes do not read the counter; this hook never allocates.
/// # Arguments
/// * `token` - Exact published wait identity.
/// # Returns
/// Returns after TimerPublished, or unchanged for inactive/stale/foreign waits.
pub(crate) fn wait_published(token: WaitToken) {
    let mut state = slot(token.cpu()).lock();
    if let Some(s) = state.session.as_mut() {
        if let Some(c) = &s.cycle {
            if c.token == Some(token) {
                s.stamp(c.index, Phase::TimerPublished, None);
            }
        }
    }
}

/// Observe committed blocking state before saving the outgoing context.
/// Uses only the leaf lock, without allocation or scheduler lookup.
/// # Arguments
/// * `owner` - Committing thread captured under scheduler ownership.
/// # Returns
/// Returns after WaitCommitted for the owner's bound cycle, or unchanged otherwise.
#[inline(always)]
pub(crate) fn sleep_committed(owner: ThreadId) {
    sleep_phase(owner, Phase::WaitCommitted);
}

/// Observe completion of the outgoing sleep's context handoff preparation.
/// Uses only the leaf lock, without allocation or scheduler lookup.
/// # Arguments
/// * `owner` - Outgoing thread captured under scheduler ownership, not current lookup.
/// # Returns
/// Returns after SleepHandoff for the owner's bound cycle, or unchanged otherwise.
#[inline(always)]
pub(crate) fn sleep_handoff(owner: ThreadId) {
    sleep_phase(owner, Phase::SleepHandoff);
}

// Scheduler commit already verified the bound wait. Both outgoing boundaries
// require its exact owner, not an expired cycle or the newly current thread.
fn sleep_phase(owner: ThreadId, phase: Phase) {
    let mut state = slot(cpu()).lock();
    if let Some(s) = state.session.as_mut() {
        if s.owner == owner {
            if let Some(c) = &s.cycle {
                if c.token.is_some() {
                    s.stamp(c.index, phase, None);
                }
            }
        }
    }
}

/// Retain actual programmed CVAL only when the nearest deadline belongs to the
/// pending profiled wait. Unrelated timer programming cannot overwrite it.
/// # Arguments
/// * `requested` - Nearest time-owned deadline sent to architecture.
/// * `effective` - Actual CVAL after architecture past-deadline clamping.
/// # Returns
/// Returns after bounded metadata update, without counter reads/allocation.
pub fn programmed(requested: u64, effective: u64) {
    let mut state = slot(cpu()).lock();
    if let Some(s) = state.session.as_mut() {
        if s.active {
            if let Some(c) = &s.cycle {
                let r = &mut s.records[c.index];
                if !c.expired && c.token.is_some() && r.requested_deadline == requested {
                    r.programmed_deadline = effective;
                }
            }
        }
    }
}

/// Stage the common live IRQ entry, clearing prior acknowledgement and event
/// association. No cycle is stamped until exact expiration; no callbacks.
/// # Arguments
/// * `entry` - Earliest assembly probe read only from live exception padding.
/// # Returns
/// Returns after bounded staging without another counter read.
pub fn irq_entry(entry: u64) {
    let mut state = slot(cpu()).lock();
    state.irq_record = None;
    state.irq_entry = entry;
    state.ack = None;
}

/// Stage common controller acknowledgement after GIC/Pi acknowledge returns.
/// Takes only the leaf lock, without allocation or scheduler calls.
/// # Returns
/// Returns after staging a raw counter only if Acknowledge is selected.
pub fn irq_ack() {
    let mut state = slot(cpu()).lock();
    state.ack = state
        .session
        .as_ref()
        .filter(|s| s.active && s.cycle.is_some() && s.selected(Phase::Acknowledge))
        .map(|_| counter());
}

/// Attach IRQ probes at extraction of the exact token and deadline.
/// # Arguments
/// * `token` - Extracted time-owned wait identity including immutable CPU.
/// * `deadline` - Original queue deadline, not the IRQ sampling time.
/// # Returns
/// Returns after first exact association; stale/duplicate events ignored.
pub(crate) fn expired(token: WaitToken, deadline: u64) {
    let mut state = slot(token.cpu()).lock();
    let entry = state.irq_entry;
    let ack = state.ack;
    if let Some(s) = state.session.as_mut() {
        if let Some(index) = s.expired(token, deadline, entry, ack) {
            state.irq_record = Some(index);
        }
    }
}

/// Observe scheduler acquisition for dispatch of the originating timer IRQ.
/// Takes only the profiler leaf lock; allocates nothing.
/// # Returns
/// Returns after SchedulerAcquired, or unchanged if no exact wait expired in IRQ.
#[inline(always)]
pub(crate) fn scheduler_acquired() {
    irq_phase(Phase::SchedulerAcquired);
}

/// Observe completed rearm bookkeeping for the originating timer IRQ.
/// Takes only the profiler leaf lock; allocates nothing.
/// # Returns
/// Returns after TimerRearmed, or unchanged if no exact wait expired in IRQ.
#[inline(always)]
pub(crate) fn timer_rearmed() {
    irq_phase(Phase::TimerRearmed);
}

/// Observe platform controller completion for the originating IRQ.
/// Takes only the profiler leaf lock; allocates nothing.
/// # Returns
/// Returns after InterruptCompleted, or unchanged if no exact wait expired in IRQ.
#[inline(always)]
pub fn interrupt_completed() {
    irq_phase(Phase::InterruptCompleted);
}

// Only exact expiry associates a record with this IRQ. Later unrelated IRQs
// cannot fill or replace its originating dispatch/completion boundaries.
fn irq_phase(phase: Phase) {
    let mut state = slot(cpu()).lock();
    if let Some(index) = state.irq_record {
        if let Some(s) = state.session.as_mut() {
            s.stamp(index, phase, None);
        }
    }
}

/// Observe Running/current commitment inside the scheduler transition.
/// Called before invariant validation, outgoing context save, or incoming restore,
/// including delayed generic selection after expiry.
/// Takes only the profiler leaf lock; allocates nothing and preserves IRQ evidence.
/// # Arguments
/// * `owner` - Thread just committed as the CPU's running resume target.
/// * `path` - Direct idle wake or shared scheduler handoff classification.
/// # Returns
/// Returns after WakeSelected; unrelated/nonexpired cycles are ignored.
#[inline(always)]
pub(crate) fn wake_selected(owner: ThreadId, path: WakePath) {
    resume(owner, Phase::WakeSelected, path == WakePath::Idle);
}

/// Observe the selected owner's restored context and activated address space.
/// Takes only the profiler leaf lock; allocates nothing and retains wake classification.
/// # Arguments
/// * `owner` - Actual restored thread captured under scheduler ownership.
/// # Returns
/// Returns after ContextRestored; unrelated/nonexpired cycles are ignored.
#[inline(always)]
pub(crate) fn context_restored(owner: ThreadId) {
    resume(owner, Phase::ContextRestored, false);
}

fn resume(owner: ThreadId, phase: Phase, idle: bool) {
    let mut state = slot(cpu()).lock();
    if let Some(s) = state.session.as_mut() {
        s.resume(owner, phase, idle);
    }
}

/// Final live EL0 return probe, called before assembly GPR restoration.
/// Captures current identity before locking. Saved-context first entry never
/// invokes this hook; it needs no exception padding or frame dereference.
/// # Returns
/// Returns after selected probe and lifecycle completion for exact owner only.
pub fn return_to_user() {
    let Some(owner) = sched::current_thread_id() else {
        return;
    };
    let mut state = slot(cpu()).lock();
    if let Some(s) = state.session.as_mut() {
        s.returned(owner);
    }
}

#[cfg(test)]
mod tests;
