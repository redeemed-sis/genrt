use crate::{
    arch::ActiveContext,
    config::KERNEL_CPU_CAPACITY,
    cpu::{self, CpuId},
    sched::{ThreadId, WaitToken},
    sync::IrqSpinLock,
};
use alloc::vec::Vec;

unsafe extern "C" {
    fn arch_counter_now() -> u64;
    fn arch_counter_freq_hz() -> u64;
    fn arch_timer_arm_deadline(deadline: u64);
    fn arch_timer_disarm();
}

/// Preallocated timed-event slots reserved for each scheduler thread slot.
///
/// One thread may concurrently own one exact wait deadline and one scheduler
/// quantum. The third slot preserves the configured bounded queue headroom;
/// scheduler bootstrap multiplies this constant by thread capacity.
pub(crate) const TIMED_EVENT_CAPACITY_PER_THREAD: usize = 3;

/// Exact deadline identity stored in one logical CPU's bounded time queue.
///
/// Event construction and comparison are copy-only. Queue mutation and timer
/// programming occur only through the time subsystem APIs below.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum TimedEvent {
    /// Time-owned deadline for one exact externally published wait.
    WaitDeadline { cpu: CpuId, token: WaitToken },
    /// Round-robin quantum owned by one CPU-local scheduler context.
    QuantumExpired { cpu: CpuId, thread: ThreadId },
}

impl TimedEvent {
    /// Construct a deadline event for one exact scheduler wait.
    ///
    /// # Arguments
    ///
    /// * `token` - Exact wait registration whose immutable home CPU owns the
    ///   resulting event.
    ///
    /// # Returns
    ///
    /// Returns a copyable event identity. Construction does not allocate,
    /// block, or alter IRQ, timer, or scheduler state.
    pub(crate) const fn wait_deadline(token: WaitToken) -> Self {
        Self::WaitDeadline {
            cpu: token.cpu(),
            token,
        }
    }

    /// Construct a quantum-expiration event for one running thread.
    ///
    /// # Arguments
    ///
    /// * `cpu` - Logical CPU whose scheduler context and timer own the event.
    /// * `thread` - Generation-aware running thread identity.
    ///
    /// # Returns
    ///
    /// Returns a copyable event identity. Construction does not allocate,
    /// block, or alter IRQ, timer, or scheduler state.
    pub(crate) const fn quantum_expired(cpu: CpuId, thread: ThreadId) -> Self {
        Self::QuantumExpired { cpu, thread }
    }

    const fn cpu(self) -> CpuId {
        match self {
            Self::WaitDeadline { cpu, .. } | Self::QuantumExpired { cpu, .. } => cpu,
        }
    }

    fn sort_key(self) -> (u8, usize, u32, u64) {
        match self {
            Self::WaitDeadline { token, .. } => (
                0,
                token.thread().index(),
                token.thread().generation(),
                token.sequence(),
            ),
            Self::QuantumExpired { thread, .. } => (1, thread.index(), thread.generation(), 0),
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct DeadlineEntry {
    deadline: u64,
    event: TimedEvent,
}

struct DeadlineQueue {
    entries: Vec<DeadlineEntry>,
}

impl DeadlineQueue {
    fn with_capacity(capacity: usize) -> Self {
        let mut entries = Vec::new();
        entries.reserve_exact(capacity);
        Self { entries }
    }

    fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    fn schedule(&mut self, deadline: u64, event: TimedEvent) {
        if let Some(index) = self.find_event_index(event) {
            let old_deadline = self.entries[index].deadline;
            self.entries[index].deadline = deadline;
            if deadline < old_deadline {
                self.sift_up(index);
            } else if deadline > old_deadline {
                self.sift_down(index);
            }
            return;
        }

        if self.entries.len() == self.entries.capacity() {
            panic!("time: deadline queue capacity exhausted");
        }

        self.entries.push(DeadlineEntry { deadline, event });
        let last = self.entries.len() - 1;
        self.sift_up(last);
    }

    fn cancel(&mut self, event: TimedEvent) -> bool {
        let Some(index) = self.find_event_index(event) else {
            return false;
        };

        self.remove_at(index);
        true
    }

    fn event_pending(&self, event: TimedEvent) -> bool {
        self.find_event_index(event).is_some()
    }

    fn pop_expired(&mut self, now: u64) -> Option<TimedEvent> {
        let entry = self.entries.first().copied()?;
        if entry.deadline > now {
            return None;
        }

        Some(self.remove_at(0).event)
    }

    fn next_deadline(&self) -> Option<u64> {
        self.entries.first().map(|entry| entry.deadline)
    }

    fn find_event_index(&self, event: TimedEvent) -> Option<usize> {
        self.entries.iter().position(|entry| entry.event == event)
    }

    fn remove_at(&mut self, index: usize) -> DeadlineEntry {
        let last_index = self.entries.len() - 1;
        self.entries.swap(index, last_index);
        let removed = self
            .entries
            .pop()
            .unwrap_or_else(|| panic!("time: empty deadline queue"));

        if index < self.entries.len() {
            if index > 0 && self.less(index, Self::parent(index)) {
                self.sift_up(index);
            } else {
                self.sift_down(index);
            }
        }

        removed
    }

    fn sift_up(&mut self, mut index: usize) {
        while index > 0 {
            let parent = Self::parent(index);
            if !self.less(index, parent) {
                break;
            }
            self.entries.swap(index, parent);
            index = parent;
        }
    }

    fn sift_down(&mut self, mut index: usize) {
        loop {
            let left = Self::left(index);
            if left >= self.entries.len() {
                break;
            }

            let right = left + 1;
            let mut best = left;
            if right < self.entries.len() && self.less(right, left) {
                best = right;
            }

            if !self.less(best, index) {
                break;
            }

            self.entries.swap(index, best);
            index = best;
        }
    }

    fn less(&self, lhs: usize, rhs: usize) -> bool {
        let left = self.entries[lhs];
        let right = self.entries[rhs];
        left.deadline < right.deadline
            || (left.deadline == right.deadline && left.event.sort_key() < right.event.sort_key())
    }

    const fn parent(index: usize) -> usize {
        (index - 1) / 2
    }

    const fn left(index: usize) -> usize {
        (index * 2) + 1
    }
}

struct TimeState {
    // Each logical CPU owns one independent instance of this state:
    // - registration/cancellation/update,
    // - nearest-deadline selection,
    // - that CPU's one-shot timer reprogramming,
    // - expired-event dispatch on timer IRQ.
    //
    // The deadline queue is heap-backed but fully reserved during bootstrap so
    // timer IRQ handling never grows it at runtime.
    queue: DeadlineQueue,
    armed_timer_deadline: Option<u64>,
    dispatching_irq: bool,
}

impl TimeState {
    fn new(deadline_capacity: usize) -> Self {
        Self {
            queue: DeadlineQueue::with_capacity(deadline_capacity),
            armed_timer_deadline: None,
            dispatching_irq: false,
        }
    }

    fn schedule_event(&mut self, deadline: u64, event: TimedEvent) {
        self.queue.schedule(deadline, event);
    }

    fn cancel_event(&mut self, event: TimedEvent) -> bool {
        self.queue.cancel(event)
    }

    fn event_pending(&self, event: TimedEvent) -> bool {
        self.queue.event_pending(event)
    }

    fn pop_expired(&mut self, now: u64) -> Option<TimedEvent> {
        self.queue.pop_expired(now)
    }

    fn rearm_timer(&mut self, now: u64) {
        let next_deadline = self.queue.next_deadline();
        if next_deadline != self.armed_timer_deadline {
            match next_deadline {
                Some(deadline) => crate::trace!("time: arm next deadline={deadline} now={now}"),
                None => crate::trace!("time: disarm timer"),
            }
        }

        program_timer_deadline(next_deadline);
        self.armed_timer_deadline = next_deadline;
    }
}

// Every queue is independently bounded and protected. Runtime APIs select a
// queue by explicit event ownership or by the executing logical CPU; only the
// owner CPU may program the corresponding architected physical timer.
static CPU_TIME: [IrqSpinLock<Option<TimeState>>; KERNEL_CPU_CAPACITY] =
    [const { IrqSpinLock::new(None) }; KERNEL_CPU_CAPACITY];

#[inline(always)]
pub fn now_counter() -> u64 {
    // SAFETY: the architecture layer exposes a monotonic hardware counter.
    unsafe { arch_counter_now() }
}

#[inline(always)]
pub fn counter_freq_hz() -> u64 {
    // SAFETY: the architecture layer exposes the architected timer frequency.
    unsafe { arch_counter_freq_hz() }
}

#[inline(always)]
pub fn ns_to_counts(ns: u64) -> u64 {
    scale_to_counts(ns, 1_000_000_000)
}

#[inline(always)]
pub fn us_to_counts(us: u64) -> u64 {
    scale_to_counts(us, 1_000_000)
}

#[inline(always)]
pub fn ms_to_counts(ms: u64) -> u64 {
    scale_to_counts(ms, 1_000)
}

#[inline(always)]
pub fn uptime_ms() -> u64 {
    now_counter() / ms_to_counts(1)
}

/// Initialize the executing CPU's bounded deadline queue and local timer.
///
/// Scheduler bootstrap calls this only after publishing shared scheduler
/// lifecycle state. Capacity reservation allocates during bootstrap; runtime
/// schedule, cancel, and IRQ dispatch paths never grow the queue.
///
/// # Arguments
///
/// * `deadline_capacity` - Maximum number of timed events owned concurrently
///   by the executing logical CPU.
///
/// # Returns
///
/// Returns after publishing an empty queue and disarming the executing CPU's
/// architected timer.
///
/// # Panics
///
/// Panics if the executing CPU is not registered, its queue is already
/// initialized, or bootstrap allocation cannot reserve the requested capacity.
pub(crate) fn init_current_cpu(deadline_capacity: usize) {
    let cpu = current_cpu();
    let mut slot = time_slot(cpu).lock();
    if slot.is_some() {
        panic!("time: CPU{} already initialized", cpu.index());
    }

    let state = TimeState::new(deadline_capacity);
    let capacity = state.queue.capacity();
    program_timer_deadline(None);
    *slot = Some(state);
    drop(slot);
    crate::debug!(
        "time: CPU{} deadline queue capacity={capacity}",
        cpu.index()
    );
}

/// Schedule or update an event on the executing CPU's deadline queue.
///
/// This bounded runtime path takes the owner queue's IRQ-safe lock and may
/// reprogram only the executing CPU's architected timer. It never allocates.
/// Remote insertion is rejected until an IPI-backed remote timer command
/// exists.
///
/// # Arguments
///
/// * `deadline` - Absolute architecture counter value at which the event is
///   eligible for dispatch.
/// * `event` - Exact event identity whose owner must be the executing CPU.
///
/// # Returns
///
/// Returns after the event is present and the local timer reflects the nearest
/// deadline.
///
/// # Panics
///
/// Panics if CPU identity cannot be resolved, the event belongs to another
/// CPU, the local queue is uninitialized, or its reserved capacity is exhausted.
pub(crate) fn schedule_event(deadline: u64, event: TimedEvent) {
    let cpu = current_cpu();
    if event.cpu() != cpu {
        panic!(
            "time: remote schedule from CPU{} to CPU{} requires IPI support",
            cpu.index(),
            event.cpu().index()
        );
    }
    let now = now_counter();
    with_time_mut(cpu, |time| {
        time.schedule_event(deadline, event);
        if !time.dispatching_irq {
            time.rearm_timer(now);
        }
    });
    crate::trace!("time: scheduled {event:?} deadline={deadline}");
}

/// Cancel an exact event from its owning CPU queue.
///
/// Cancellation may be requested by another CPU after an external condition
/// wins a timed wait. The target queue mutation is synchronized, but only the
/// owner CPU may touch its physical timer. A remote cancellation can therefore
/// leave one harmless early timer interrupt; that interrupt observes the
/// updated queue and rearms locally. This path is bounded and allocation-free.
///
/// # Arguments
///
/// * `event` - Exact event identity, including its owning logical CPU.
///
/// # Returns
///
/// Returns after removing the event when present. Missing, already-dispatched,
/// and stale events are controlled no-ops.
///
/// # Panics
///
/// Panics if current CPU identity cannot be resolved or the owner queue is not
/// initialized.
pub(crate) fn cancel_event(event: TimedEvent) {
    let current = current_cpu();
    let owner = event.cpu();
    let now = now_counter();
    let canceled = with_time_mut(owner, |time| {
        let canceled = time.cancel_event(event);
        if current == owner && !time.dispatching_irq {
            time.rearm_timer(now);
        }
        canceled
    });
    if canceled {
        crate::trace!("time: canceled {event:?}");
    }
}

/// Test whether an exact event remains queued on its owner CPU.
///
/// # Arguments
///
/// * `event` - Exact event identity, including its owning logical CPU.
///
/// # Returns
///
/// Returns `true` when the owner queue still contains the event, or `false`
/// after cancellation or dispatch. The bounded query allocates nothing.
///
/// # Panics
///
/// Panics if the owner queue is not initialized.
pub(crate) fn event_pending(event: TimedEvent) -> bool {
    with_time(event.cpu(), |time| time.event_pending(event))
}

/// Dispatch expired timed events and complete scheduler IRQ-return handoff.
///
/// This IRQ path uses only bounded, preallocated state. It does not allocate,
/// block, or extend the local IRQ-disabled interval with parsing, user copies,
/// or resource destruction.
///
/// # Arguments
///
/// * `context` - Exclusive live IRQ return context that the scheduler may save
///   and replace after timed-event dispatch.
///
/// # Returns
///
/// Returns after all events expired at the sampled counter value are handled,
/// the scheduler handoff is committed, and the one-shot timer is rearmed.
pub fn on_timer_interrupt(context: &mut ActiveContext<'_>) {
    #[cfg(feature = "qemu-test-kernel-runtime")]
    crate::test_support::kernel_runtime::note_timer_irq();

    let cpu = current_cpu();
    if time_slot(cpu).lock().is_none() {
        // Keep stray early-boot timer IRQs from ever reaching scheduler
        // state before this CPU's queue is initialized.
        program_timer_deadline(None);
        return;
    }

    // Timer IRQ fast-path policy: do not allocate here. The heap is protected
    // against local IRQ reentrancy for ordinary thread-context allocations, but
    // timed-event dispatch itself must stay on preallocated, bounded state.
    let now = now_counter();
    with_time_mut(cpu, |time| {
        time.dispatching_irq = true;
    });

    // Do not retain the time owner across scheduler dispatch: completion can
    // acquire scheduler state and potentially select another return context.
    while let Some(event) = with_time_mut(cpu, |time| time.pop_expired(now)) {
        dispatch_expired_event(cpu, event);
    }

    crate::sched::finish_timer_interrupt(context, now);

    with_time_mut(cpu, |time| {
        time.dispatching_irq = false;
        time.rearm_timer(now);
    });
}

#[inline(always)]
fn dispatch_expired_event(cpu: CpuId, event: TimedEvent) {
    if event.cpu() != cpu {
        panic!(
            "time: CPU{} dequeued event owned by CPU{}",
            cpu.index(),
            event.cpu().index()
        );
    }
    match event {
        TimedEvent::WaitDeadline { token, .. } => {
            crate::trace!("time: dispatch WaitDeadline({token:?})");
            crate::sched::on_wait_deadline(token);
        }
        TimedEvent::QuantumExpired {
            thread: thread_id, ..
        } => {
            crate::trace!("time: dispatch QuantumExpired({thread_id})");
            crate::sched::on_quantum_expired(thread_id);
        }
    }
}

#[inline(always)]
fn program_timer_deadline(deadline: Option<u64>) {
    match deadline {
        Some(deadline) => {
            // SAFETY: time owns the earliest absolute deadline in counter units.
            unsafe { arch_timer_arm_deadline(deadline) }
        }
        None => {
            // SAFETY: time explicitly disables the timer when no deadlines remain.
            unsafe { arch_timer_disarm() }
        }
    }
}

#[inline(always)]
fn with_time_mut<R>(cpu: CpuId, f: impl FnOnce(&mut TimeState) -> R) -> R {
    let mut time = time_slot(cpu).lock();
    f(time
        .as_mut()
        .unwrap_or_else(|| panic!("time: CPU{} is not initialized", cpu.index())))
}

#[inline(always)]
fn with_time<R>(cpu: CpuId, f: impl FnOnce(&TimeState) -> R) -> R {
    let time = time_slot(cpu).lock();
    f(time
        .as_ref()
        .unwrap_or_else(|| panic!("time: CPU{} is not initialized", cpu.index())))
}

#[inline(always)]
fn time_slot(cpu: CpuId) -> &'static IrqSpinLock<Option<TimeState>> {
    CPU_TIME
        .get(cpu.index())
        .unwrap_or_else(|| panic!("time: invalid CPU{} queue", cpu.index()))
}

#[inline(always)]
fn current_cpu() -> CpuId {
    cpu::current_id().unwrap_or_else(|err| panic!("time: current CPU lookup failed: {err:?}"))
}

#[inline(always)]
fn scale_to_counts(units: u64, denom_per_second: u64) -> u64 {
    if units == 0 {
        return 0;
    }

    let freq = counter_freq_hz().max(1) as u128;
    let numerator = (units as u128).saturating_mul(freq);
    let counts = div_ceil(numerator, denom_per_second as u128);
    counts.max(1).min(u64::MAX as u128) as u64
}

#[inline(always)]
fn div_ceil(numerator: u128, denominator: u128) -> u128 {
    numerator.div_ceil(denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_queues_keep_cpu_owned_events_separate() {
        let cpu0 = CpuId::from_index(0).unwrap();
        let cpu1 = CpuId::from_index(1).unwrap();
        let thread = ThreadId::new(3, 7);
        let event0 = TimedEvent::quantum_expired(cpu0, thread);
        let event1 = TimedEvent::quantum_expired(cpu1, thread);
        let mut queue0 = DeadlineQueue::with_capacity(1);
        let mut queue1 = DeadlineQueue::with_capacity(1);

        queue0.schedule(10, event0);
        queue1.schedule(20, event1);

        assert!(queue0.event_pending(event0));
        assert!(!queue0.event_pending(event1));
        assert!(queue1.event_pending(event1));
        assert!(!queue1.event_pending(event0));
    }
}
