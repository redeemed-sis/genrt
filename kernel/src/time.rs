use core::cell::UnsafeCell;

unsafe extern "C" {
    fn arch_counter_now() -> u64;
    fn arch_counter_freq_hz() -> u64;
    fn arch_timer_arm_deadline(deadline: u64);
    fn arch_timer_disarm();
}

// Current static system shape:
// - up to 8 tasks,
// - one wake event per sleeping task,
// - one outstanding quantum event per runnable task at most.
const MAX_TIMED_EVENTS: usize = 16;
pub(crate) type TimedTaskId = usize;
type FinishTimerInterruptHandler = fn(*mut u64, u64);
type TimedTaskHandler = fn(TimedTaskId);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum TimedEvent {
    WakeTask(TimedTaskId),
    QuantumExpired(TimedTaskId),
}

#[derive(Copy, Clone)]
pub(crate) struct TimeHandlers {
    // Scheduler-owned reactions injected during bootstrap so `kernel::time`
    // stays agnostic of the scheduler module itself.
    pub finish_timer_interrupt: FinishTimerInterruptHandler,
    pub wake_task: TimedTaskHandler,
    pub quantum_expired: TimedTaskHandler,
}

#[derive(Copy, Clone)]
struct TimedSlot {
    event: Option<TimedEvent>,
    deadline: u64,
}

impl TimedSlot {
    const fn empty() -> Self {
        Self {
            event: None,
            deadline: 0,
        }
    }
}

struct TimeState {
    // `kernel::time` is the sole owner of timed events:
    // - registration/cancellation,
    // - nearest-deadline selection,
    // - one-shot timer reprogramming,
    // - expired-event dispatch on timer IRQ.
    slots: [TimedSlot; MAX_TIMED_EVENTS],
    armed_timer_deadline: Option<u64>,
    dispatching_irq: bool,
    handlers: Option<TimeHandlers>,
}

impl TimeState {
    const fn new() -> Self {
        Self {
            slots: [TimedSlot::empty(); MAX_TIMED_EVENTS],
            armed_timer_deadline: None,
            dispatching_irq: false,
            handlers: None,
        }
    }

    fn reset(&mut self) {
        for slot in &mut self.slots {
            *slot = TimedSlot::empty();
        }

        self.armed_timer_deadline = None;
        self.dispatching_irq = false;
    }

    fn schedule_event(&mut self, deadline: u64, event: TimedEvent) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.event == Some(event)) {
            slot.deadline = deadline;
            return;
        }

        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.event.is_none())
            .unwrap_or_else(|| panic!("time: no free timed event slots"));
        slot.event = Some(event);
        slot.deadline = deadline;
    }

    fn cancel_event(&mut self, event: TimedEvent) -> bool {
        let Some(slot) = self.slots.iter_mut().find(|slot| slot.event == Some(event)) else {
            return false;
        };

        *slot = TimedSlot::empty();
        true
    }

    fn event_pending(&self, event: TimedEvent) -> bool {
        self.slots.iter().any(|slot| slot.event == Some(event))
    }

    fn collect_expired(&mut self, now: u64) -> [Option<TimedEvent>; MAX_TIMED_EVENTS] {
        let mut expired = [None; MAX_TIMED_EVENTS];
        let mut out = 0usize;

        for slot in &mut self.slots {
            let Some(event) = slot.event else {
                continue;
            };

            if now < slot.deadline {
                continue;
            }

            expired[out] = Some(event);
            out += 1;
            *slot = TimedSlot::empty();
        }

        expired
    }

    fn next_deadline(&self) -> Option<u64> {
        self.slots
            .iter()
            .filter_map(|slot| slot.event.map(|_| slot.deadline))
            .min()
    }

    fn rearm_timer(&mut self, now: u64) {
        let next_deadline = self.next_deadline();
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

struct TimeCell(UnsafeCell<TimeState>);

// SAFETY: genrt currently mutates time state only on a single core.
unsafe impl Sync for TimeCell {}

static TIME: TimeCell = TimeCell(UnsafeCell::new(TimeState::new()));

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

pub(crate) fn init(handlers: TimeHandlers) {
    let time = time_mut();
    time.reset();
    time.handlers = Some(handlers);
    program_timer_deadline(None);
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

pub fn on_timer_interrupt(active_frame_words: *mut u64) {
    if active_frame_words.is_null() {
        return;
    }

    let now = now_counter();
    let expired = {
        let time = time_mut();
        time.dispatching_irq = true;
        time.collect_expired(now)
    };
    let handlers = time_ref()
        .handlers
        .unwrap_or_else(|| panic!("time: handlers are not initialized"));

    for event in expired.into_iter().flatten() {
        dispatch_expired_event(handlers, event);
    }

    (handlers.finish_timer_interrupt)(active_frame_words, now);

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
fn time_mut() -> &'static mut TimeState {
    // SAFETY: Access is single-writer in the current single-core bring-up model.
    unsafe { &mut *TIME.0.get() }
}

#[inline(always)]
fn time_ref() -> &'static TimeState {
    // SAFETY: Access is single-core; read-only borrow does not outlive this call.
    unsafe { &*TIME.0.get() }
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
