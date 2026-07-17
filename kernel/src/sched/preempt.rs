use crate::{
    arch::ActiveContext,
    task::{TaskId, ThreadId},
    time::TimedEvent,
};

use super::{
    IDLE_TASK_ID, Scheduler, WaitCause, WaitToken, complete_wait, log_switch, scheduler_mut,
    transition::{SwitchOutcome, TaskState, ThreadKind},
};

/// Finish one bounded timer IRQ and optionally replace its return context.
pub(super) fn finish_timer_interrupt(context: &mut ActiveContext<'_>, now: u64) {
    scheduler_mut().finish_timer_interrupt(context, now);
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
    scheduler_mut().note_quantum_expired(thread);
}

pub fn current_task_id() -> Option<TaskId> {
    scheduler_mut()
        .running_task()
        .map(|id| TaskId::new(id.index()))
}

/// Validate bounded scheduler ownership after a test-controlled transition.
///
/// This test-only seam performs no allocation or scheduler mutation. It masks
/// local IRQs while walking the table so timer dispatch cannot interleave with
/// validation; task preemption state is otherwise unchanged.
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
/// This requests, but does not itself require, a task switch. The controlled
/// task-call checkpoint services the request when preemption is enabled; under
/// `crate::sync::preempt::PreemptGuard` it returns with the request pending for the
/// outermost guard drop. This operation allocates nothing and does not block.
///
/// # Returns
///
/// Returns after the checkpoint either services the request or leaves it
/// pending because preemption remains disabled.
pub fn yield_now() {
    crate::sync::preempt::request_reschedule();
    crate::task_call::preempt_checkpoint();
}

pub(crate) fn enter_running_task() -> ! {
    scheduler_mut().enter_running_task()
}

/// Service one private EL1 preemption checkpoint against the active context.
///
/// This is a bounded, allocation-free scheduler safe point. It may replace
/// `context` only after consuming a pending request at depth zero.
///
/// # Arguments
///
/// * `context` - Exclusive live EL1 task-call context available for optional
///   saved-context handoff.
///
/// # Returns
///
/// Returns after retaining the current context or replacing it with the next
/// ready task. It does not create a new reschedule request.
pub(crate) fn on_preempt_checkpoint(context: &mut ActiveContext<'_>) {
    scheduler_mut().preempt_checkpoint(context);
}

impl Scheduler {
    pub(super) fn finish_timer_interrupt(&mut self, context: &mut ActiveContext<'_>, now: u64) {
        if !self.entered_running_task {
            return;
        }

        // Scheduler IRQ-return handoff policy: this path must remain heap-free.
        // The kernel heap is IRQ-safe for task-context allocation, but the
        // timer/scheduler fast path must continue to run on preallocated state.
        let current = match self.running_task() {
            Some(id) => id,
            None => return,
        };
        let must_leave_idle = current.index() == IDLE_TASK_ID.index() && self.has_runnable_peer();
        let mut refreshed_quantum_event = false;

        if must_leave_idle {
            crate::sync::preempt::request_reschedule();
        }

        // Safe point: timer IRQ return may replace the active frame, but only
        // after the deferred-preemption state confirms depth zero. Timed-event
        // dispatch and timer rearm continue even while a task holds a guard.
        if crate::sync::preempt::checkpoint_consume() {
            self.handoff_optional(context);

            self.replace_quantum_event(now, current);
            refreshed_quantum_event = true;
        }

        if !refreshed_quantum_event {
            self.ensure_quantum_event(now);
        }
    }

    fn preempt_checkpoint(&mut self, context: &mut ActiveContext<'_>) {
        // Safe point: the private EL1 task-call checkpoint owns the optional
        // task-context handoff. It acknowledges only an existing request.
        if !crate::sync::preempt::checkpoint_consume() {
            return;
        }

        let Some(current) = self.running_task() else {
            return;
        };
        self.handoff_optional(context);
        self.replace_quantum_event(crate::time::now_counter(), current);
    }

    pub(super) fn finish_block_current(&mut self, current: ThreadId, next: ThreadId) {
        // A mandatory handoff also acknowledges any stale request at its
        // depth-zero checkpoint; it never needs to select a second task.
        let _ = crate::sync::preempt::checkpoint_consume();
        let now = crate::time::now_counter();
        self.replace_quantum_event(now, current);
        if next != current {
            log_switch(TaskId::new(current.index()), TaskId::new(next.index()));
        }
    }

    pub(super) fn enter_running_task(&mut self) -> ! {
        let running = self
            .running_task()
            .unwrap_or_else(|| panic!("scheduler has no running task"));
        let now = crate::time::now_counter();

        self.entered_running_task = true;
        self.ensure_quantum_event(now);
        debug_assert_eq!(self.current_thread(), Some(running));
        debug_assert_eq!(
            self.task_state(TaskId::new(running.index())),
            TaskState::Running
        );
        crate::sync::preempt::mark_scheduler_online();
        self.activate_task_address_space(running);
        self.saved_context(running).enter()
    }

    pub(super) fn running_task(&self) -> Option<super::ThreadId> {
        self.current_thread()
    }

    pub(super) fn note_quantum_expired(&mut self, thread: ThreadId) {
        if self.running_task() == Some(thread) {
            crate::sync::preempt::request_reschedule();
            crate::trace!("sched: quantum expired task {thread}");
        }
    }

    pub(super) fn activate_task_address_space(&self, id: super::ThreadId) {
        let result = match self.task_kind(TaskId::new(id.index())) {
            ThreadKind::Kernel => unsafe { crate::memory::vm::clear_user_address_space() },
            ThreadKind::User { address_space, .. } => unsafe {
                crate::memory::vm::activate_user_address_space(address_space)
            },
        };

        if let Err(err) = result {
            panic!(
                "sched: failed to activate address space for task {}: {err:?}",
                id.index()
            );
        }
    }

    pub(super) fn has_runnable_peer(&self) -> bool {
        self.transition_has_ready()
    }

    pub(super) fn note_runnable_peer_available(&mut self) {
        if self.current_thread().is_some() && self.has_runnable_peer() {
            crate::sync::preempt::request_reschedule();
            self.ensure_quantum_event(crate::time::now_counter());
        }
    }

    fn ensure_quantum_event(&mut self, now: u64) {
        let Some(current) = self.running_task() else {
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
            "sched: task {current} quantum={}ms until counter {deadline}",
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
        self.activate_task_address_space(to);
        log_switch(TaskId::new(from.index()), TaskId::new(to.index()));
    }
}
