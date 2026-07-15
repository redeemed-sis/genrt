use alloc::vec::Vec;
use core::cell::UnsafeCell;

use crate::{arch::ActiveContext, task::TaskId};

unsafe extern "C" {
    fn arch_counter_now() -> u64;
    fn arch_counter_freq_hz() -> u64;
    fn arch_timer_arm_deadline(deadline: u64);
    fn arch_timer_disarm();
}

type FinishTimerInterruptHandler = fn(&mut ActiveContext<'_>, u64);
type TimedTaskHandler = fn(TaskId);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum TimedEvent {
    WakeTask(TaskId),
    QuantumExpired(TaskId),
    // IPC timeouts are typed task events, not callbacks. Scheduler owns the
    // task wait state and asks IPC to remove the mailbox waiter during dispatch.
    IpcTimeout(TaskId),
}

impl TimedEvent {
    fn sort_key(self) -> (u8, usize) {
        match self {
            Self::WakeTask(task_id) => (0, task_id.index()),
            Self::QuantumExpired(task_id) => (1, task_id.index()),
            Self::IpcTimeout(task_id) => (2, task_id.index()),
        }
    }
}

#[derive(Copy, Clone)]
pub(crate) struct TimeHandlers {
    // Scheduler-owned reactions injected during bootstrap so `kernel::time`
    // stays agnostic of the scheduler module itself.
    pub finish_timer_interrupt: FinishTimerInterruptHandler,
    pub wake_task: TimedTaskHandler,
    pub quantum_expired: TimedTaskHandler,
    pub ipc_timeout: TimedTaskHandler,
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

    fn reset(&mut self) {
        self.entries.clear();
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
    // `kernel::time` remains the sole owner of timed events:
    // - registration/cancellation/update,
    // - nearest-deadline selection,
    // - one-shot timer reprogramming,
    // - expired-event dispatch on timer IRQ.
    //
    // The deadline queue is heap-backed but fully reserved during bootstrap so
    // timer IRQ handling never grows it at runtime.
    queue: DeadlineQueue,
    armed_timer_deadline: Option<u64>,
    dispatching_irq: bool,
    handlers: TimeHandlers,
}

impl TimeState {
    fn new(handlers: TimeHandlers, deadline_capacity: usize) -> Self {
        Self {
            queue: DeadlineQueue::with_capacity(deadline_capacity),
            armed_timer_deadline: None,
            dispatching_irq: false,
            handlers,
        }
    }

    fn reset(&mut self) {
        self.queue.reset();
        self.armed_timer_deadline = None;
        self.dispatching_irq = false;
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

struct TimeCell(UnsafeCell<Option<TimeState>>);

// SAFETY: genrt currently mutates time state only on a single core.
unsafe impl Sync for TimeCell {}

static TIME: TimeCell = TimeCell(UnsafeCell::new(None));

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

pub(crate) fn init(handlers: TimeHandlers, deadline_capacity: usize) {
    let slot = time_slot_mut();
    if slot.is_some() {
        panic!("time: already initialized");
    }

    // Scheduler bootstrap publishes the global `Scheduler` before calling
    // `time::init()`, so any timer callback installed here can safely resolve
    // scheduler-owned state once timer IRQ dispatch becomes active.
    let mut state = TimeState::new(handlers, deadline_capacity);
    state.reset();
    crate::debug!("time: deadline queue capacity={}", state.queue.capacity());
    program_timer_deadline(None);
    *slot = Some(state);
}

pub(crate) fn schedule_event(deadline: u64, event: TimedEvent) {
    let now = now_counter();
    let time = time_mut();
    time.schedule_event(deadline, event);
    crate::trace!("time: scheduled {event:?} deadline={deadline}");
    if !time.dispatching_irq {
        time.rearm_timer(now);
    }
}

pub(crate) fn cancel_event(event: TimedEvent) {
    let now = now_counter();
    let time = time_mut();
    if time.cancel_event(event) {
        crate::trace!("time: canceled {event:?}");
        if !time.dispatching_irq {
            time.rearm_timer(now);
        }
    }
}

pub(crate) fn event_pending(event: TimedEvent) -> bool {
    time_ref().event_pending(event)
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
    if try_time_mut().is_none() {
        // Keep stray early-boot timer IRQs from ever reaching scheduler
        // callbacks before `time::init()` installs their handler table.
        program_timer_deadline(None);
        return;
    }

    // Timer IRQ fast-path policy: do not allocate here. The heap is protected
    // against local IRQ reentrancy for ordinary task-context allocations, but
    // timed-event dispatch itself must stay on preallocated, bounded state.
    let now = now_counter();
    let handlers = {
        let time = time_mut();
        time.dispatching_irq = true;
        time.handlers
    };

    while let Some(event) = time_mut().pop_expired(now) {
        dispatch_expired_event(handlers, event);
    }

    (handlers.finish_timer_interrupt)(context, now);

    let time = time_mut();
    time.dispatching_irq = false;
    time.rearm_timer(now);
}

#[inline(always)]
fn dispatch_expired_event(handlers: TimeHandlers, event: TimedEvent) {
    match event {
        TimedEvent::WakeTask(task_id) => {
            crate::trace!("time: dispatch WakeTask({task_id})");
            (handlers.wake_task)(task_id);
        }
        TimedEvent::QuantumExpired(task_id) => {
            crate::trace!("time: dispatch QuantumExpired({task_id})");
            (handlers.quantum_expired)(task_id);
        }
        TimedEvent::IpcTimeout(task_id) => {
            crate::debug!("time: timeout fired IpcTimeout({task_id})");
            (handlers.ipc_timeout)(task_id);
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
fn time_slot_mut() -> &'static mut Option<TimeState> {
    // SAFETY: Access is single-writer in the current single-core bring-up model.
    unsafe { &mut *TIME.0.get() }
}

#[inline(always)]
fn time_mut() -> &'static mut TimeState {
    time_slot_mut()
        .as_mut()
        .unwrap_or_else(|| panic!("time: subsystem is not initialized"))
}

#[inline(always)]
fn try_time_mut() -> Option<&'static mut TimeState> {
    time_slot_mut().as_mut()
}

#[inline(always)]
fn time_ref() -> &'static TimeState {
    time_slot_mut()
        .as_ref()
        .unwrap_or_else(|| panic!("time: subsystem is not initialized"))
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
