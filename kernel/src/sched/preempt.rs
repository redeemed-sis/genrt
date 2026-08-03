use crate::{arch::ActiveContext, time::TimedEvent};

use super::{
    CpuScheduler, ThreadId, WaitCause, WaitToken, complete_wait, current_cpu, log_switch,
    scheduler_mut,
    transition::{SwitchOutcome, ThreadState},
};

/// Finish one bounded timer IRQ and optionally replace its return context.
pub(super) fn finish_timer_interrupt(context: &mut ActiveContext<'_>, now: u64) {
    let cpu = current_cpu();
    scheduler_mut()
        .on_cpu(cpu)
        .finish_timer_interrupt(context, now);
}

/// Complete one time-owned exact wait deadline.
///
/// # Arguments
///
/// * `token` - Exact wait registration stored in the expired timed event.
///
/// # Returns
///
/// Returns after a bounded completion attempt. Stale or already-completed
/// deadlines are controlled no-ops.
pub(super) fn on_wait_deadline(token: WaitToken) {
    let _ = complete_wait(token, WaitCause::Timeout);
}

pub(super) fn on_quantum_expired(thread: ThreadId) {
    let cpu = current_cpu();
    scheduler_mut().on_cpu(cpu).note_quantum_expired(thread);
}

/// Validate bounded scheduler ownership after a test-controlled transition.
///
/// This test-only seam performs no allocation or scheduler mutation. It masks
/// local IRQs while walking the table so timer dispatch cannot interleave with
/// validation; thread preemption state is otherwise unchanged.
///
/// # Returns
///
/// Returns after checking current identity, lifecycle/context ownership, and
/// ready-queue membership.
///
/// # Panics
///
/// Panics if scheduler lifecycle, context ownership, current identity, or
/// ready-queue membership violates an invariant.
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) fn validate_invariants_for_test() {
    let _irq_guard = crate::sync::LocalIrqGuard::save_and_disable();
    scheduler_mut().validate_invariants();
}

/// Request one cooperative scheduler checkpoint.
///
/// This requests, but does not itself require, a thread switch. The controlled
/// sched-call checkpoint services the request when preemption is enabled; under
/// `crate::sync::preempt::PreemptGuard` it returns with the request pending for the
/// outermost guard drop. This operation allocates nothing and does not block.
///
/// # Returns
///
/// Returns after the checkpoint either services the request or leaves it
/// pending because preemption remains disabled.
pub fn yield_now() {
    crate::sync::preempt::request_reschedule();
    crate::sched::call::preempt_checkpoint();
}

/// Enter the currently selected scheduler thread context.
///
/// This never returns. It marks scheduler runtime online, may activate or
/// clear TTBR0, and enters the saved architecture context without allocating
/// or blocking. It must not be called from IRQ context.
///
/// # Returns
///
/// Never returns because the selected saved context is entered.
///
/// # Panics
///
/// Panics when bootstrap did not select a running thread or architecture
/// address-space activation fails.
pub(crate) fn enter_running_thread() -> ! {
    let cpu = current_cpu();
    scheduler_mut().on_cpu(cpu).enter_running_thread()
}

/// Service one private EL1 preemption checkpoint against the active context.
///
/// This is a bounded, allocation-free scheduler safe point. It may replace
/// `context` only after consuming a pending request at depth zero.
///
/// # Arguments
///
/// * `context` - Exclusive live EL1 sched-call context available for optional
///   saved-context handoff.
///
/// # Returns
///
/// Returns after retaining the current context or replacing it with the next
/// ready thread. It does not create a new reschedule request.
pub(crate) fn on_preempt_checkpoint(context: &mut ActiveContext<'_>) {
    let cpu = current_cpu();
    scheduler_mut().on_cpu(cpu).preempt_checkpoint(context);
}

impl CpuScheduler<'_> {
    pub(super) fn finish_timer_interrupt(&mut self, context: &mut ActiveContext<'_>, now: u64) {
        if !self.state().online {
            return;
        }

        // Scheduler IRQ-return handoff policy: this path must remain heap-free.
        // The kernel heap is IRQ-safe for thread-context allocation, but the
        // timer/scheduler fast path must continue to run on preallocated state.
        let current = match self.running_thread() {
            Some(id) => id,
            None => return,
        };
        let must_leave_idle = self.state().idle == Some(current) && self.has_runnable_peer();
        let mut refreshed_quantum_event = false;

        if must_leave_idle {
            self.state_mut().preemption.request();
        }

        // Safe point: timer IRQ return may replace the active frame, but only
        // after the deferred-preemption state confirms depth zero. Timed-event
        // dispatch and timer rearm continue even while a thread holds a guard.
        let online = self.state().online;
        if self.state_mut().preemption.consume_checkpoint(online) {
            self.handoff_optional(context);

            self.replace_quantum_event(now, current);
            refreshed_quantum_event = true;
        }

        if !refreshed_quantum_event {
            self.ensure_quantum_event(now);
        }
    }

    fn preempt_checkpoint(&mut self, context: &mut ActiveContext<'_>) {
        // Safe point: the private EL1 sched-call checkpoint owns the optional
        // thread-context handoff. It acknowledges only an existing request.
        let online = self.state().online;
        if !self.state_mut().preemption.consume_checkpoint(online) {
            return;
        }

        let Some(current) = self.running_thread() else {
            return;
        };
        self.handoff_optional(context);
        self.replace_quantum_event(crate::time::now_counter(), current);
    }

    pub(super) fn finish_block_current(&mut self, current: ThreadId, next: ThreadId) {
        // A mandatory handoff also acknowledges any stale request at its
        // depth-zero checkpoint; it never needs to select a second thread.
        let online = self.state().online;
        let _ = self.state_mut().preemption.consume_checkpoint(online);
        let now = crate::time::now_counter();
        self.replace_quantum_event(now, current);
        if next != current {
            log_switch(self.cpu(), current, next);
        }
    }

    pub(super) fn enter_running_thread(&mut self) -> ! {
        let running = self
            .running_thread()
            .unwrap_or_else(|| panic!("scheduler has no running thread"));
        let now = crate::time::now_counter();

        if self.state().online {
            panic!("sched: CPU{} entered twice", self.cpu().index());
        }
        self.ensure_quantum_event(now);
        debug_assert_eq!(self.current_thread(), Some(running));
        debug_assert_eq!(self.thread_state(running.index()), ThreadState::Running);
        self.activate_thread_address_space(running);
        self.state_mut().online = true;
        self.saved_context(running).enter()
    }

    pub(super) fn running_thread(&self) -> Option<super::ThreadId> {
        self.current_thread()
    }

    pub(super) fn note_quantum_expired(&mut self, thread: ThreadId) {
        if self.running_thread() == Some(thread) {
            self.state_mut().preemption.request();
            crate::trace!("sched: quantum expired thread {thread}");
        }
    }

    pub(super) fn activate_thread_address_space(&self, id: super::ThreadId) {
        let result = match self.thread_address_space(id.index()) {
            None => unsafe { crate::memory::vm::clear_user_address_space() },
            Some(address_space) => unsafe {
                crate::memory::vm::activate_user_address_space(address_space)
            },
        };

        if let Err(err) = result {
            panic!(
                "sched: failed to activate address space for thread {}: {err:?}",
                id.index()
            );
        }
    }

    pub(super) fn has_runnable_peer(&self) -> bool {
        self.transition_has_ready()
    }

    pub(super) fn note_runnable_peer_available(&mut self) {
        if self.current_thread().is_some() && self.has_runnable_peer() {
            self.state_mut().preemption.request();
            self.ensure_quantum_event(crate::time::now_counter());
        }
    }

    fn ensure_quantum_event(&mut self, now: u64) {
        let Some(current) = self.running_thread() else {
            return;
        };

        let event = TimedEvent::QuantumExpired(current);
        if !self.has_runnable_peer() {
            crate::time::cancel_event(event);
            return;
        }

        if crate::time::event_pending(event) {
            return;
        }

        let deadline = now.wrapping_add(crate::time::ms_to_counts(self.rr_quantum_ms));
        crate::time::schedule_event(deadline, event);
        crate::trace!(
            "sched: thread {current} quantum={}ms until counter {deadline}",
            self.rr_quantum_ms
        );
    }

    pub(super) fn replace_quantum_event(&mut self, now: u64, obsolete_thread: ThreadId) {
        crate::time::cancel_event(TimedEvent::QuantumExpired(obsolete_thread));
        self.ensure_quantum_event(now);
    }

    fn handoff_optional(&mut self, context: &mut ActiveContext<'_>) {
        let SwitchOutcome::Switch { from, to } = self.transition_optional_switch() else {
            return;
        };
        self.saved_context_mut(from).save_from(context);
        self.saved_context(to).restore_into(context);
        self.activate_thread_address_space(to);
        log_switch(self.cpu(), from, to);
    }
}
