use crate::{
    arch::ActiveContext,
    config::{KERNEL_CPU_CAPACITY, KERNEL_THREAD_CAPACITY},
    cpu::{self, CpuId},
    sched::{ThreadId, WaitToken},
    sync::IrqSpinLock,
};
use alloc::{boxed::Box, vec::Vec};
#[cfg(feature = "qemu-test-smp-boot")]
use core::sync::atomic::{AtomicUsize, Ordering};

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
/// CPU-local time initialization multiplies this constant by configured thread
/// capacity before any local timer IRQ is enabled.
pub(crate) const TIMED_EVENT_CAPACITY_PER_THREAD: usize = 3;

/// Errors from publishing one CPU's bounded deadline queue.
///
/// These diagnostics are copy-only and never carry allocator-owned state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum TimeInitError {
    /// Requested topology is empty or exceeds fixed CPU capacity.
    InvalidCpuCount,
    /// The executing CPU already owns a published time state.
    AlreadyInitialized,
    /// Configured thread/event capacity overflowed `usize`.
    CapacityOverflow,
    /// Bootstrap allocation could not reserve the bounded queue storage.
    OutOfMemory,
}

/// Errors from converting between the architecture counter and civil units.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum TimeConversionError {
    /// The architecture reported a zero counter frequency.
    InvalidFrequency,
    /// The requested duration or absolute timestamp does not fit in `u64`
    /// counter units.
    Overflow,
}

/// Normalized monotonic timestamp used at the syscall boundary.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct MonotonicTime {
    /// Whole seconds since the unspecified monotonic epoch.
    pub(crate) seconds: u64,
    /// Fractional nanoseconds in the range `0..1_000_000_000`.
    pub(crate) nanoseconds: u32,
}

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
    fn try_with_capacity(capacity: usize) -> Result<Self, TimeInitError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| TimeInitError::OutOfMemory)?;
        Ok(Self { entries })
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
    expired: Option<ExpiredEvents>,
}

/// Fixed CPU-owned transfer buffer borrowed for one timer IRQ transaction.
///
/// Only its initialized prefix is exposed to the scheduler. Storage is
/// allocated during bootstrap, transferred out of time ownership during
/// dispatch, and returned before the timer is rearmed; it cannot grow.
pub(crate) struct ExpiredEvents {
    entries: Box<[Option<TimedEvent>]>,
    len: usize,
}

impl ExpiredEvents {
    fn try_new(capacity: usize) -> Result<Self, TimeInitError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| TimeInitError::OutOfMemory)?;
        entries.resize(capacity, None);
        Ok(Self {
            entries: entries.into_boxed_slice(),
            len: 0,
        })
    }

    fn push(&mut self, event: TimedEvent) {
        let slot = self
            .entries
            .get_mut(self.len)
            .unwrap_or_else(|| panic!("time: expired buffer capacity exhausted"));
        *slot = Some(event);
        self.len += 1;
    }

    /// Iterate the exact expired-event prefix without taking time ownership.
    ///
    /// # Returns
    ///
    /// Returns a bounded iterator of copied events. This query allocates
    /// nothing, does not block, and never accesses the mutable deadline queue.
    pub(crate) fn iter(&self) -> impl Iterator<Item = TimedEvent> + '_ {
        self.entries[..self.len]
            .iter()
            .map(|event| event.expect("time: empty expired slot"))
    }

    /// Query whether this batch contains exactly one expired event.
    ///
    /// # Returns
    ///
    /// Returns its copied identity, or `None` for empty or multi-event batches.
    /// This bounded query allocates nothing and takes no time ownership.
    pub(crate) fn single(&self) -> Option<TimedEvent> {
        (self.len == 1).then(|| self.entries[0].expect("time: empty expired slot"))
    }
}

/// Bounded quantum intent returned after scheduler timer ownership releases.
///
/// The time owner applies cancellation before ensuring the selected thread's
/// deadline. No timer or queue lock needs to cross the timer scheduler handoff.
pub(crate) struct QuantumUpdate {
    /// Exact obsolete quantum to remove, if a checkpoint refreshed scheduling.
    pub(crate) obsolete: Option<TimedEvent>,
    /// Candidate deadline inserted only if its exact event is not pending.
    pub(crate) ensure: Option<(u64, TimedEvent)>,
}

impl TimeState {
    fn try_new(deadline_capacity: usize) -> Result<Self, TimeInitError> {
        Ok(Self {
            queue: DeadlineQueue::try_with_capacity(deadline_capacity)?,
            armed_timer_deadline: None,
            dispatching_irq: false,
            expired: Some(ExpiredEvents::try_new(deadline_capacity)?),
        })
    }

    fn schedule_event(&mut self, deadline: u64, event: TimedEvent) {
        self.queue.schedule(deadline, event);
    }

    fn cancel_event(&mut self, event: TimedEvent, rearm_at: Option<u64>) -> bool {
        let canceled = self.queue.cancel(event);
        // A missing quantum must not reprogram the surviving wait deadline.
        // Remote callers and IRQ batch cleanup defer programming to the owner.
        if canceled
            && !self.dispatching_irq
            && let Some(now) = rearm_at
        {
            self.rearm_timer(now);
        }
        canceled
    }

    fn event_pending(&self, event: TimedEvent) -> bool {
        self.queue.event_pending(event)
    }

    fn take_expired(&mut self, cpu: CpuId, now: u64) -> ExpiredEvents {
        self.dispatching_irq = true;
        let mut expired = self
            .expired
            .take()
            .unwrap_or_else(|| panic!("time: reentrant timer dispatch"));
        while let Some(event) = self.queue.pop_expired(now) {
            if event.cpu() != cpu {
                panic!("time: expired event belongs to another CPU");
            }
            expired.push(event);
        }
        expired
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

#[cfg(feature = "qemu-test-smp-boot")]
static TIMER_PROBE_COMPLETIONS: [AtomicUsize; KERNEL_CPU_CAPACITY] =
    [const { AtomicUsize::new(0) }; KERNEL_CPU_CAPACITY];

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

/// Convert an architecture counter observation to normalized monotonic time.
///
/// # Arguments
///
/// * `counter` - Absolute observation in the architecture counter domain.
///
/// # Returns
///
/// Returns whole seconds and normalized fractional nanoseconds. Conversion is
/// bounded, allocation-free, non-blocking, and leaves IRQ state unchanged.
///
/// # Errors
///
/// Returns [`TimeConversionError::InvalidFrequency`] if the architecture
/// reports a zero frequency.
pub(crate) fn counter_to_monotonic_time(
    counter: u64,
) -> Result<MonotonicTime, TimeConversionError> {
    counter_to_monotonic_time_at_frequency(counter, counter_freq_hz())
}

/// Convert a normalized monotonic timestamp to an absolute counter deadline.
///
/// The conversion rounds upward so a timer programmed with the result cannot
/// expire before the requested timestamp.
///
/// # Arguments
///
/// * `seconds` - Whole non-negative seconds since the monotonic epoch.
/// * `nanoseconds` - Fractional nanoseconds, which must be below one billion.
///
/// # Returns
///
/// Returns the first counter value at or after the requested timestamp. The
/// conversion is bounded, allocation-free, non-blocking, and leaves IRQ state
/// unchanged.
///
/// # Errors
///
/// Returns [`TimeConversionError::InvalidFrequency`] for a zero architecture
/// frequency and [`TimeConversionError::Overflow`] for invalid nanoseconds or
/// a timestamp beyond the `u64` counter domain.
pub(crate) fn monotonic_time_to_counter(
    seconds: u64,
    nanoseconds: u32,
) -> Result<u64, TimeConversionError> {
    monotonic_time_to_counter_at_frequency(seconds, nanoseconds, counter_freq_hz())
}

/// Convert a relative duration to counter ticks, rounding upward.
///
/// # Arguments
///
/// * `seconds` - Whole non-negative duration seconds.
/// * `nanoseconds` - Fractional duration nanoseconds below one billion.
///
/// # Returns
///
/// Returns the smallest counter duration that is not shorter than the input.
/// Zero maps to zero. The conversion does not allocate, block, or alter IRQ
/// state.
///
/// # Errors
///
/// Returns [`TimeConversionError::InvalidFrequency`] for a zero architecture
/// frequency and [`TimeConversionError::Overflow`] for invalid nanoseconds or
/// a duration beyond the `u64` counter domain.
pub(crate) fn monotonic_duration_to_counts(
    seconds: u64,
    nanoseconds: u32,
) -> Result<u64, TimeConversionError> {
    monotonic_time_to_counter(seconds, nanoseconds)
}

/// Return the effective nanosecond resolution of the monotonic counter.
///
/// # Returns
///
/// Returns a non-zero normalized duration derived from one architecture
/// counter tick. Sub-nanosecond counters report one nanosecond because that is
/// the finest value representable by the userspace ABI. The query is bounded,
/// allocation-free, non-blocking, and leaves IRQ state unchanged.
///
/// # Errors
///
/// Returns [`TimeConversionError::InvalidFrequency`] if the architecture
/// reports a zero frequency.
pub(crate) fn monotonic_resolution() -> Result<MonotonicTime, TimeConversionError> {
    let frequency = counter_freq_hz();
    if frequency == 0 {
        return Err(TimeConversionError::InvalidFrequency);
    }
    let nanoseconds = div_ceil(1_000_000_000u128, frequency as u128).max(1);
    Ok(MonotonicTime {
        seconds: (nanoseconds / 1_000_000_000) as u64,
        nanoseconds: (nanoseconds % 1_000_000_000) as u32,
    })
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

/// Preallocate and publish bounded deadline queues for the boot CPU topology.
///
/// CPU0 calls this once after publishing shared scheduler lifecycle state and
/// before starting secondary CPUs. All capacity reservation therefore occurs
/// on CPU0; secondary bring-up and every runtime schedule, cancel, and IRQ path
/// remain allocation-free. Architecture-local timer registers are initialized
/// separately by each executing CPU.
///
/// # Arguments
///
/// * `cpu_count` - Number of logical CPU slots described by the accepted boot
///   topology.
///
/// # Returns
///
/// Returns `Ok(())` after publishing one empty fixed-capacity queue for every
/// CPU in the topology.
///
/// # Errors
///
/// Returns [`TimeInitError`] when the topology is invalid, capacity arithmetic
/// or reservation fails, or any selected CPU slot is already initialized.
pub(crate) fn init_cpu_states(cpu_count: usize) -> Result<(), TimeInitError> {
    if cpu_count == 0 || cpu_count > KERNEL_CPU_CAPACITY {
        return Err(TimeInitError::InvalidCpuCount);
    }
    let deadline_capacity = KERNEL_THREAD_CAPACITY
        .checked_mul(TIMED_EVENT_CAPACITY_PER_THREAD)
        .ok_or(TimeInitError::CapacityOverflow)?;
    for slot in &CPU_TIME[..cpu_count] {
        if slot.lock().is_some() {
            return Err(TimeInitError::AlreadyInitialized);
        }
    }

    let mut prepared: [Option<TimeState>; KERNEL_CPU_CAPACITY] = core::array::from_fn(|_| None);
    for state in &mut prepared[..cpu_count] {
        *state = Some(TimeState::try_new(deadline_capacity)?);
    }
    for (slot, state) in CPU_TIME[..cpu_count]
        .iter()
        .zip(prepared[..cpu_count].iter_mut())
    {
        *slot.lock() = state.take();
    }

    crate::debug!("time: initialized {cpu_count} CPU queue(s), capacity={deadline_capacity} each");
    Ok(())
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
            "time: remote schedule from CPU{} to CPU{} requires remote timer command support",
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
        time.cancel_event(event, (current == owner).then_some(now))
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
///
/// # Panics
///
/// Panics for invalid CPU/event ownership, nested timer dispatch, buffer
/// exhaustion, or a scheduler handoff invariant violation.
pub fn on_timer_interrupt(context: &mut ActiveContext<'_>) {
    let cpu = current_cpu();
    crate::trace!("time: timer IRQ cpu={}", cpu.index());

    #[cfg(feature = "qemu-test-kernel-runtime")]
    crate::test_support::kernel_runtime::note_timer_irq();

    let mut owner = time_slot(cpu).lock();
    let Some(time) = owner.as_mut() else {
        // Keep stray early-boot timer IRQs from ever reaching scheduler
        // state before this CPU's queue is initialized.
        program_timer_deadline(None);
        return;
    };

    // Timer IRQ fast-path policy: do not allocate here. The heap is protected
    // against local IRQ reentrancy for ordinary thread-context allocations, but
    // timed-event dispatch itself must stay on preallocated, bounded state.
    let now = now_counter();
    let mut expired = time.take_expired(cpu, now);
    drop(owner);

    let quantum = crate::sched::finish_timer_interrupt(context, now, &expired);

    with_time_mut(cpu, |time| {
        if let Some(event) = quantum.obsolete {
            time.cancel_event(event, None);
        }
        if let Some((deadline, event)) = quantum.ensure {
            if !time.event_pending(event) {
                time.schedule_event(deadline, event);
            }
        }
        expired.len = 0;
        time.expired = Some(expired);
        time.dispatching_irq = false;
        time.rearm_timer(now);
    });
}

/// Record one fully acknowledged and EOI-completed local timer probe IRQ.
///
/// The AArch64 dispatcher calls this only after writing GICC_EOIR. Scheduler
/// idle quanta supply the recurring owner-local deadlines, so observing two or
/// more completions proves that the executing CPU can accept another local PPI
/// after EOI without a test-only timer command.
///
/// # Returns
///
/// Returns after one bounded atomic update. The IRQ-context path allocates
/// nothing and does not enter the scheduler.
#[cfg(feature = "qemu-test-smp-boot")]
pub fn on_local_timer_probe_eoi_for_test() {
    let cpu = current_cpu();
    TIMER_PROBE_COMPLETIONS[cpu.index()].fetch_add(1, Ordering::AcqRel);
}

/// Test whether every expected CPU completed two local timer probe IRQs.
///
/// # Arguments
///
/// * `cpu_count` - Number of registered logical CPUs expected by the QEMU case.
///
/// # Returns
///
/// Returns `true` only when each expected CPU reports at least two completed
/// acknowledge/EOI cycles. The bounded query allocates nothing.
#[cfg(feature = "qemu-test-smp-boot")]
pub(crate) fn local_timer_probes_complete_for_test(cpu_count: usize) -> bool {
    cpu_count <= KERNEL_CPU_CAPACITY
        && TIMER_PROBE_COMPLETIONS[..cpu_count]
            .iter()
            .all(|count| count.load(Ordering::Acquire) >= 2)
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

fn counter_to_monotonic_time_at_frequency(
    counter: u64,
    frequency: u64,
) -> Result<MonotonicTime, TimeConversionError> {
    if frequency == 0 {
        return Err(TimeConversionError::InvalidFrequency);
    }
    let seconds = counter / frequency;
    let remainder = counter % frequency;
    let nanoseconds = ((remainder as u128) * 1_000_000_000u128 / frequency as u128) as u32;
    Ok(MonotonicTime {
        seconds,
        nanoseconds,
    })
}

fn monotonic_time_to_counter_at_frequency(
    seconds: u64,
    nanoseconds: u32,
    frequency: u64,
) -> Result<u64, TimeConversionError> {
    if frequency == 0 {
        return Err(TimeConversionError::InvalidFrequency);
    }
    if nanoseconds >= 1_000_000_000 {
        return Err(TimeConversionError::Overflow);
    }

    let whole = seconds
        .checked_mul(frequency)
        .ok_or(TimeConversionError::Overflow)?;
    // Normal counter frequencies fit this product in u64, avoiding a software
    // 128-bit divide in the IRQ-masked sleep syscall. Keep the wider fallback.
    let fractional = match (nanoseconds as u64).checked_mul(frequency) {
        Some(product) => product.div_ceil(1_000_000_000),
        None => u64::try_from(div_ceil(
            (nanoseconds as u128) * frequency as u128,
            1_000_000_000,
        ))
        .map_err(|_| TimeConversionError::Overflow)?,
    };
    whole
        .checked_add(fractional)
        .ok_or(TimeConversionError::Overflow)
}

#[inline(always)]
fn div_ceil(numerator: u128, denominator: u128) -> u128 {
    numerator.div_ceil(denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cancellation_keeps_the_armed_timer_untouched() {
        let cpu = CpuId::from_index(0).unwrap();
        let thread = ThreadId::new(1, 1);
        let event = TimedEvent::quantum_expired(cpu, thread);
        let missing = TimedEvent::quantum_expired(cpu, ThreadId::new(2, 1));
        let mut time = TimeState::try_new(2).unwrap();
        time.schedule_event(100, event);
        time.rearm_timer(0);
        let before = crate::test_arch_stubs::timer_program_counts();

        assert!(!time.cancel_event(missing, Some(0)));
        assert_eq!(time.queue.next_deadline(), Some(100));
        assert_eq!(time.armed_timer_deadline, Some(100));
        assert_eq!(crate::test_arch_stubs::timer_program_counts(), before);

        assert!(time.cancel_event(event, Some(0)));
        assert_eq!(time.queue.next_deadline(), None);
        assert_eq!(time.armed_timer_deadline, None);
        assert_eq!(
            crate::test_arch_stubs::timer_program_counts(),
            (before.0, before.1 + 1)
        );
    }

    #[test]
    fn remote_and_batched_cancellation_defer_timer_programming() {
        let cpu = CpuId::from_index(0).unwrap();
        let event = TimedEvent::quantum_expired(cpu, ThreadId::new(1, 1));
        for (dispatching, rearm_at) in [(false, None), (true, Some(0))] {
            let mut time = TimeState::try_new(1).unwrap();
            time.schedule_event(100, event);
            time.rearm_timer(0);
            time.dispatching_irq = dispatching;
            let before = crate::test_arch_stubs::timer_program_counts();
            assert!(time.cancel_event(event, rearm_at));
            assert_eq!(time.queue.next_deadline(), None);
            assert_eq!(time.armed_timer_deadline, Some(100));
            assert_eq!(crate::test_arch_stubs::timer_program_counts(), before);
        }
    }

    #[test]
    fn expired_transfer_is_bounded_and_reuses_storage() {
        let cpu = CpuId::from_index(0).unwrap();
        let events = core::array::from_fn::<_, 3, _>(|index| {
            TimedEvent::quantum_expired(cpu, ThreadId::new(index, 1))
        });
        let mut time = TimeState::try_new(3).unwrap();
        for (index, event) in events.into_iter().enumerate() {
            time.schedule_event(10 + index as u64, event);
        }
        let storage = time.expired.as_ref().unwrap().entries.as_ptr();
        let mut batch = time.take_expired(cpu, 10);
        assert_eq!(batch.single(), Some(events[0]));
        assert_eq!(batch.iter().collect::<Vec<_>>(), [events[0]]);
        assert!(time.expired.is_none());
        assert_eq!(time.queue.next_deadline(), Some(11));
        batch.len = 0;
        time.expired = Some(batch);
        let mut batch = time.take_expired(cpu, 12);
        assert_eq!(batch.entries.as_ptr(), storage);
        assert_eq!(batch.single(), None);
        assert_eq!(batch.iter().collect::<Vec<_>>(), events[1..]);
        assert_eq!(time.queue.next_deadline(), None);
        batch.len = 0;
        time.expired = Some(batch);
        let batch = time.take_expired(cpu, 100);
        assert_eq!(batch.single(), None);
        assert_eq!(batch.iter().count(), 0);
        assert_eq!(batch.entries.as_ptr(), storage);
    }

    #[test]
    #[should_panic(expected = "expired buffer capacity exhausted")]
    fn expired_transfer_cannot_grow() {
        let mut batch = ExpiredEvents::try_new(1).unwrap();
        let event = TimedEvent::quantum_expired(CpuId::from_index(0).unwrap(), ThreadId::new(1, 1));
        batch.push(event);
        batch.push(event);
    }

    #[test]
    #[should_panic(expected = "reentrant timer dispatch")]
    fn expired_transfer_has_one_owner() {
        let cpu = CpuId::from_index(0).unwrap();
        let mut time = TimeState::try_new(1).unwrap();
        let _batch = time.take_expired(cpu, 0);
        let _second = time.take_expired(cpu, 0);
    }

    #[test]
    fn topology_init_preallocates_every_selected_cpu_queue() {
        assert_eq!(init_cpu_states(0), Err(TimeInitError::InvalidCpuCount));
        assert_eq!(init_cpu_states(2), Ok(()));

        let expected_capacity = KERNEL_THREAD_CAPACITY * TIMED_EVENT_CAPACITY_PER_THREAD;
        for slot in &CPU_TIME[..2] {
            let state = slot.lock();
            assert_eq!(
                state.as_ref().map(|time| time.queue.entries.capacity()),
                Some(expected_capacity)
            );
        }
        assert!(CPU_TIME[2].lock().is_none());
        assert_eq!(init_cpu_states(2), Err(TimeInitError::AlreadyInitialized));
    }

    #[test]
    fn deadline_queues_keep_cpu_owned_events_separate() {
        let cpu0 = CpuId::from_index(0).unwrap();
        let cpu1 = CpuId::from_index(1).unwrap();
        let thread = ThreadId::new(3, 7);
        let event0 = TimedEvent::quantum_expired(cpu0, thread);
        let event1 = TimedEvent::quantum_expired(cpu1, thread);
        let mut queue0 = DeadlineQueue::try_with_capacity(1).expect("queue0 allocation");
        let mut queue1 = DeadlineQueue::try_with_capacity(1).expect("queue1 allocation");

        queue0.schedule(10, event0);
        queue1.schedule(20, event1);

        assert!(queue0.event_pending(event0));
        assert!(!queue0.event_pending(event1));
        assert!(queue1.event_pending(event1));
        assert!(!queue1.event_pending(event0));
    }

    #[test]
    fn counter_time_conversion_is_normalized_and_rounds_deadlines_up() {
        assert_eq!(
            counter_to_monotonic_time_at_frequency(1_500_001, 1_000_000),
            Ok(MonotonicTime {
                seconds: 1,
                nanoseconds: 500_001_000,
            })
        );
        assert_eq!(
            monotonic_time_to_counter_at_frequency(1, 1, 1_000_000),
            Ok(1_000_001)
        );
    }

    #[test]
    fn checked_time_conversion_rejects_invalid_or_unrepresentable_values() {
        assert_eq!(
            counter_to_monotonic_time_at_frequency(1, 0),
            Err(TimeConversionError::InvalidFrequency)
        );
        assert_eq!(
            monotonic_time_to_counter_at_frequency(0, 1_000_000_000, 1),
            Err(TimeConversionError::Overflow)
        );
        assert_eq!(
            monotonic_time_to_counter_at_frequency(u64::MAX, 0, 2),
            Err(TimeConversionError::Overflow)
        );
    }

    #[test]
    fn native_deadline_conversion_matches_the_wide_reference() {
        for frequency in [
            1,
            19_200_000,
            62_500_000,
            1_000_000_000,
            u32::MAX as u64,
            u32::MAX as u64 + 1,
            u64::MAX / 999_999_999,
            u64::MAX / 999_999_999 + 1,
            u64::MAX,
        ] {
            for seconds in [0, 1, 7, u64::MAX / frequency, u64::MAX] {
                for nanoseconds in [0, 1, 8_000, 500_000_000, 999_999_999] {
                    let reference = u64::try_from(
                        seconds as u128 * frequency as u128
                            + (nanoseconds as u128 * frequency as u128).div_ceil(1_000_000_000),
                    )
                    .map_err(|_| TimeConversionError::Overflow);
                    assert_eq!(
                        monotonic_time_to_counter_at_frequency(seconds, nanoseconds, frequency),
                        reference,
                        "seconds={seconds}, nanoseconds={nanoseconds}, frequency={frequency}"
                    );
                }
            }
        }
    }
}
