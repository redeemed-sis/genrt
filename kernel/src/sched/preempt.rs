use crate::{
    arch::ActiveContext,
    time::{ExpiredEvents, QuantumUpdate, TimedEvent},
};

use super::{
    CpuScheduler, ThreadId, current_cpu, log_switch, scheduler_mut,
    transition::{SwitchOutcome, ThreadState},
};

#[cfg(feature = "qemu-test-kernel-runtime")]
static DIRECT_TIMED_WAKES: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Observe completed single-wait idle handoffs in the kernel-runtime fixture.
///
/// # Returns
///
/// Returns a test-only cumulative counter. This atomic query allocates nothing,
/// does not block, and does not alter scheduler or timer ownership.
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) fn direct_timed_wakes_for_test() -> usize {
    DIRECT_TIMED_WAKES.load(core::sync::atomic::Ordering::Relaxed)
}

/// Finish one bounded timer IRQ and optionally replace its return context.
///
/// The time subsystem calls this after releasing the executing CPU's deadline
/// queue. The operation may consume one deferred reschedule request and replace
/// the live IRQ-return context, but it does not allocate.
///
/// # Arguments
///
/// * `context` - Exclusive live timer IRQ context available for an optional
///   scheduler handoff.
/// * `now` - Counter value sampled by the time subsystem for this interrupt.
/// * `expired` - Fixed batch collected after releasing the local time owner.
///
/// # Returns
///
/// Returns the bounded quantum update to apply after scheduler ownership is
/// released, retaining the current thread or installing a selected context.
///
/// # Panics
///
/// Panics if current CPU resolution, scheduler ownership, or a context-switch
/// invariant is invalid.
pub(crate) fn finish_timer_interrupt(
    context: &mut ActiveContext<'_>,
    now: u64,
    expired: &ExpiredEvents,
) -> QuantumUpdate {
    let cpu = current_cpu();
    let mut owner = scheduler_mut();
    #[cfg(feature = "wake-profile")]
    crate::profile::scheduler_acquired();
    owner
        .on_cpu(cpu)
        .finish_timer_interrupt(context, now, expired)
}

/// Finish one scheduler IPI and optionally replace its IRQ-return context.
///
/// The target CPU consumes its coalesced notification, drains all published
/// remote-ready work into the owner-local ready queue, and requests one normal
/// local scheduler checkpoint. Duplicate SGIs are harmless. The operation is
/// bounded by preallocated thread capacity and does not allocate.
///
/// # Arguments
///
/// * `context` - Exclusive live scheduler-SGI context available for an
///   optional existing scheduler handoff.
///
/// # Returns
///
/// Returns after retaining the interrupted thread or installing the selected
/// thread's saved context into `context`. GIC EOI remains architecture-owned.
///
/// # Panics
///
/// Panics if current CPU resolution, scheduler ownership, remote ingress, or a
/// context-switch invariant is invalid.
pub fn finish_scheduler_ipi(context: &mut ActiveContext<'_>) {
    let cpu = current_cpu();
    scheduler_mut().on_cpu(cpu).finish_scheduler_ipi(context);
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
    let context = {
        let mut scheduler = scheduler_mut();
        scheduler.on_cpu(cpu).prepare_running_thread_entry()
    };

    // SAFETY: the scheduler lock above published this thread as the sole
    // running owner and returned a pointer into preallocated thread storage.
    // No scheduler operation can release or replace that running slot before
    // the first context starts executing. The lock must be dropped before the
    // diverging architecture entry, otherwise it would remain held forever.
    unsafe { (&*context).enter() }
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

/// Validate that a sole current thread has no polling quantum event.
///
/// This QEMU-only check resolves the current CPU, takes bounded scheduler/time
/// ownership, and confirms that remote-work liveness does not depend on a
/// periodic scheduler deadline. It does not allocate or change scheduler state.
///
/// # Returns
///
/// Returns after validating the current thread's quantum-event absence.
///
/// # Panics
///
/// Panics when no thread is running or a quantum remains armed without a local
/// runnable peer.
#[cfg(feature = "qemu-test-smp-boot")]
pub(crate) fn validate_no_polling_quantum_for_test() {
    let cpu = current_cpu();
    let _irq_guard = crate::sync::LocalIrqGuard::save_and_disable();
    let mut scheduler = scheduler_mut();
    let local = scheduler.on_cpu(cpu);
    let current = local
        .running_thread()
        .unwrap_or_else(|| panic!("sched test: CPU{} has no current thread", cpu.index()));
    if local.has_runnable_peer()
        || crate::time::event_pending(TimedEvent::quantum_expired(cpu, current))
    {
        panic!("sched test: sole current thread retains a polling quantum");
    }
}

impl CpuScheduler<'_> {
    fn finish_scheduler_ipi(&mut self, context: &mut ActiveContext<'_>) {
        self.transition_finalize_retired();
        if !self.state().online {
            return;
        }

        self.consume_scheduler_ipi();
        self.note_runnable_peer_available();
        let online = self.state().online;
        if !crate::sync::preempt::consume_checkpoint_on(self.cpu(), online) {
            return;
        }

        let Some(current) = self.running_thread() else {
            return;
        };
        self.handoff_optional(context);
        self.replace_quantum_event(current);
    }

    pub(super) fn finish_timer_interrupt(
        &mut self,
        context: &mut ActiveContext<'_>,
        now: u64,
        expired: &ExpiredEvents,
    ) -> QuantumUpdate {
        // Complete the batch, retired joins, and return selection under one
        // scheduler owner. Local readiness requests no time lock here; one
        // quantum intent is applied by the time owner after this guard drops.
        self.transition_finalize_retired_for_timer();
        if !self.state().online {
            return QuantumUpdate {
                obsolete: None,
                ensure: None,
            };
        }
        let current = match self.running_thread() {
            Some(id) => id,
            None => {
                return QuantumUpdate {
                    obsolete: None,
                    ensure: None,
                };
            }
        };
        if let Some(TimedEvent::WaitDeadline { cpu, token }) = expired.single() {
            if cpu != self.cpu() || token.cpu() != self.cpu() {
                panic!("sched: foreign timed wait in local batch");
            }
            if !crate::sync::preempt::is_disabled()
                && let Some(outcome) = self.transition_timed_wake_from_idle(token)
            {
                // Direct selection is itself the depth-zero checkpoint; do
                // not leave a prior coalesced request to switch again later.
                let _ = crate::sync::preempt::consume_checkpoint_on(self.cpu(), true);
                self.handoff_selected(context, outcome);
                #[cfg(feature = "qemu-test-kernel-runtime")]
                DIRECT_TIMED_WAKES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                return QuantumUpdate {
                    obsolete: None,
                    ensure: None,
                };
            }
        }
        for event in expired.iter() {
            match event {
                TimedEvent::WaitDeadline { cpu, token } => {
                    if cpu != self.cpu() || token.cpu() != self.cpu() {
                        panic!("sched: foreign timed wait in local batch");
                    }
                    self.scheduler.transition_complete_wait_for_timer(token);
                }
                TimedEvent::QuantumExpired { cpu, thread } => {
                    if cpu != self.cpu() {
                        panic!("sched: foreign quantum in local batch");
                    }
                    self.note_quantum_expired(thread);
                }
            }
        }
        if self.state().idle == Some(current) && self.has_runnable_peer() {
            crate::sync::preempt::request_reschedule_on(self.cpu());
        }
        let online = self.state().online;
        let checkpoint = crate::sync::preempt::consume_checkpoint_on(self.cpu(), online);
        if checkpoint {
            self.handoff_optional(context);
        }
        let ensure = self.has_runnable_peer().then(|| {
            let thread = self
                .running_thread()
                .expect("sched: lost timer resume target");
            (
                now.wrapping_add(crate::time::ms_to_counts(self.rr_quantum_ms)),
                TimedEvent::quantum_expired(self.cpu(), thread),
            )
        });
        QuantumUpdate {
            obsolete: checkpoint.then_some(TimedEvent::quantum_expired(self.cpu(), current)),
            ensure,
        }
    }

    fn preempt_checkpoint(&mut self, context: &mut ActiveContext<'_>) {
        self.transition_finalize_retired();
        // Safe point: the private EL1 sched-call checkpoint owns the optional
        // thread-context handoff. It acknowledges only an existing request.
        let online = self.state().online;
        if !crate::sync::preempt::consume_checkpoint_on(self.cpu(), online) {
            return;
        }

        let Some(current) = self.running_thread() else {
            return;
        };
        self.handoff_optional(context);
        self.replace_quantum_event(current);
    }

    pub(super) fn finish_block_current(&mut self, current: ThreadId, next: ThreadId) {
        // A mandatory handoff also acknowledges any stale request at its
        // depth-zero checkpoint; it never needs to select a second thread.
        let online = self.state().online;
        let _ = crate::sync::preempt::consume_checkpoint_on(self.cpu(), online);
        self.replace_quantum_event(current);
        if next != current {
            log_switch(self.cpu(), current, next);
        }
    }

    /// Publish first-entry state and return the stable saved context to enter.
    ///
    /// The caller must release scheduler ownership before invoking the
    /// non-returning architecture entry on the returned pointer.
    ///
    /// # Returns
    ///
    /// Returns a stable pointer into the preallocated running thread slot.
    /// This bounded preparation does not allocate or perform the context entry.
    pub(super) fn prepare_running_thread_entry(&mut self) -> *const crate::arch::SavedContext {
        let running = self
            .running_thread()
            .unwrap_or_else(|| panic!("scheduler has no running thread"));
        if self.state().online {
            panic!("sched: CPU{} entered twice", self.cpu().index());
        }
        self.ensure_quantum_event(crate::time::now_counter);
        debug_assert_eq!(self.current_thread(), Some(running));
        debug_assert_eq!(self.thread_state(running.index()), ThreadState::Running);
        self.activate_thread_address_space(running);
        self.state_mut().online = true;
        self.saved_context(running) as *const crate::arch::SavedContext
    }

    pub(super) fn running_thread(&self) -> Option<super::ThreadId> {
        self.current_thread()
    }

    pub(super) fn note_quantum_expired(&mut self, thread: ThreadId) {
        if self.running_thread() == Some(thread) {
            crate::sync::preempt::request_reschedule_on(self.cpu());
            crate::trace!("sched: quantum expired thread {thread}");
        }
    }

    pub(super) fn activate_thread_address_space(&self, id: super::ThreadId) {
        let result = match self.thread_address_space(id.index()) {
            // Permanent idle executes only trusted high-half code and never
            // uses user pointers. Retain the installed immutable root, whose
            // VM active record continues preventing destruction. Every other
            // kernel thread detaches it; EL0 always activates its own root.
            None if self.state().idle == Some(id) => {
                crate::memory::vm::retain_user_address_space_for_idle()
            }
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
        #[cfg(feature = "qemu-test-kernel-runtime")]
        assert_eq!(
            crate::memory::vm::current_user_address_space(),
            self.thread_address_space(id.index()),
            "sched test: VM usercopy eligibility differs from selected context"
        );
    }

    pub(super) fn has_runnable_peer(&self) -> bool {
        self.transition_has_ready()
    }

    pub(super) fn note_runnable_peer_available(&mut self) {
        if self.current_thread().is_some() && self.has_runnable_peer() {
            crate::sync::preempt::request_reschedule_on(self.cpu());
            self.ensure_quantum_event(crate::time::now_counter);
        }
    }

    fn ensure_quantum_event(&mut self, now: impl FnOnce() -> u64) {
        let Some(current) = self.running_thread() else {
            return;
        };

        let event = TimedEvent::quantum_expired(self.cpu(), current);
        if !self.has_runnable_peer() {
            return;
        }
        if crate::time::event_pending(event) {
            return;
        }

        let deadline = now().wrapping_add(crate::time::ms_to_counts(self.rr_quantum_ms));
        crate::time::schedule_event(deadline, event);
        crate::trace!(
            "sched: thread {current} quantum={}ms until counter {deadline}",
            self.rr_quantum_ms
        );
    }

    /// Remove the old quantum and arm one only if the selected thread has a peer.
    /// # Arguments
    /// * `obsolete_thread` - Exact thread whose old quantum must be cancelled.
    /// # Returns
    /// Returns after bounded, allocation-free queue maintenance. The time owner
    /// masks IRQs while updating its queue; the clock is sampled only for a new
    /// quantum, after checking local peers and existing registration.
    /// # Panics
    /// Panics if the local time queue or scheduler ownership is invalid.
    pub(super) fn replace_quantum_event(&mut self, obsolete_thread: ThreadId) {
        crate::time::cancel_event(TimedEvent::quantum_expired(self.cpu(), obsolete_thread));
        self.ensure_quantum_event(crate::time::now_counter);
    }

    fn handoff_optional(&mut self, context: &mut ActiveContext<'_>) {
        let outcome = self.transition_optional_switch();
        self.handoff_selected(context, outcome);
    }

    fn handoff_selected(&mut self, context: &mut ActiveContext<'_>, outcome: SwitchOutcome) {
        let SwitchOutcome::Switch { from, to } = outcome else {
            return;
        };
        self.saved_context_mut(from).save_from(context);
        self.saved_context(to).restore_into(context);
        self.activate_thread_address_space(to);
        #[cfg(feature = "wake-profile")]
        crate::profile::context_restored(to);
        log_switch(self.cpu(), from, to);
    }
}

#[cfg(test)]
mod tests {
    use crate::sched::{Scheduler, ThreadArg};

    #[test]
    fn quantum_clock_is_not_sampled_without_a_current_thread_or_peer() {
        #[cfg(feature = "wake-profile")]
        crate::test_arch_stubs::register_profile_test_cpu();
        let cpu = crate::cpu::CpuId::from_index(0).unwrap();
        for worker in [false, true] {
            let mut scheduler = Scheduler::transition_new(2, 1);
            scheduler
                .on_cpu(cpu)
                .ensure_quantum_event(|| panic!("clock sampled without a current thread"));
            scheduler
                .on_cpu(cpu)
                .transition_append_bootstrap(|_| 0, ThreadArg::empty(), true);
            if worker {
                scheduler
                    .on_cpu(cpu)
                    .transition_append_bootstrap(|_| 0, ThreadArg::empty(), false);
            }
            scheduler.on_cpu(cpu).transition_initial_dispatch().unwrap();
            assert!(!scheduler.on_cpu(cpu).has_runnable_peer());
            scheduler
                .on_cpu(cpu)
                .ensure_quantum_event(|| panic!("clock sampled without a runnable peer"));
        }
    }
}
