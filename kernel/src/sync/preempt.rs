use core::{cell::UnsafeCell, marker::PhantomData};

use super::LocalIrqGuard;

unsafe extern "C" {
    fn arch_irq_state_allows_sched_call(saved_irq_state: u64) -> bool;
}

struct PreemptionState {
    disable_depth: usize,
    reschedule_pending: bool,
    scheduler_online: bool,
}

struct PreemptionCell(UnsafeCell<PreemptionState>);

// SAFETY: genrt's active target is single-core. Every access takes a short
// LocalIrqGuard, so thread and IRQ paths cannot concurrently mutate the state.
unsafe impl Sync for PreemptionCell {}

static PREEMPTION: PreemptionCell = PreemptionCell(UnsafeCell::new(PreemptionState {
    disable_depth: 0,
    reschedule_pending: false,
    scheduler_online: false,
}));

#[inline(always)]
fn state_mut() -> &'static mut PreemptionState {
    // SAFETY: callers hold LocalIrqGuard for the complete mutable access and
    // the active implementation has exactly one core.
    unsafe { &mut *PREEMPTION.0.get() }
}

/// Excludes thread preemption until dropped while preserving local IRQ state.
///
/// This primitive is for bootstrap or ordinary thread context only and must not
/// be used from an interrupt handler. Entry and drop mutate the private
/// single-core state under a short local-IRQ guard; neither operation allocates
/// nor blocks. An outermost drop may enter the private sched-call checkpoint.
pub(crate) struct PreemptGuard {
    _not_send: PhantomData<*mut ()>,
}

impl PreemptGuard {
    /// Enter a thread-context preemption-excluded section.
    ///
    /// # Returns
    ///
    /// Returns a non-copyable, non-send guard. Entry is bounded,
    /// allocation-free, restores the caller's IRQ state after its short state
    /// update, and panics on nesting overflow.
    ///
    /// # Panics
    ///
    /// Panics when the single-core preemption nesting counter overflows.
    pub(crate) fn enter() -> Self {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        let state = state_mut();
        state.disable_depth = state
            .disable_depth
            .checked_add(1)
            .unwrap_or_else(|| panic!("preempt: disable nesting overflow"));
        Self {
            _not_send: PhantomData,
        }
    }
}

impl Drop for PreemptGuard {
    fn drop(&mut self) {
        let irq_guard = LocalIrqGuard::save_and_disable();
        let prior_irq_allows_sched_call = {
            // SAFETY: the saved IRQ state belongs to this guard on the current
            // core. The architecture hook owns DAIF encoding details.
            unsafe { arch_irq_state_allows_sched_call(irq_guard.saved_state()) }
        };
        let checkpoint = {
            let state = state_mut();
            state.disable_depth = state
                .disable_depth
                .checked_sub(1)
                .unwrap_or_else(|| panic!("preempt: disable nesting underflow"));
            state.disable_depth == 0
                && state.reschedule_pending
                && state.scheduler_online
                && prior_irq_allows_sched_call
        };
        drop(irq_guard);

        if checkpoint {
            // The checkpoint owns pending acknowledgement and can replace the
            // active thread context. Keep no live context or raw frame pointer in
            // the guard itself.
            crate::sched::call::preempt_checkpoint();
        }
    }
}

/// Return whether thread preemption is currently excluded.
///
/// This reads private single-core state under a short local-IRQ guard. It is
/// bounded, allocation-free, and safe in thread or IRQ context.
///
/// # Returns
///
/// Returns `true` when one or more [`PreemptGuard`] instances are active.
pub(crate) fn is_disabled() -> bool {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    state_mut().disable_depth != 0
}

/// Request one coalesced scheduler checkpoint.
///
/// This operation is bounded, allocation-free, and safe in thread or IRQ
/// context. It does not itself switch threads; pending work remains until a
/// depth-zero scheduler checkpoint consumes it.
///
/// # Returns
///
/// Returns after coalescing a pending request.
pub(crate) fn request_reschedule() {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    state_mut().reschedule_pending = true;
}

/// Return whether a scheduler checkpoint is pending.
///
/// The query takes a short local-IRQ guard, allocates nothing, and does not
/// acknowledge or otherwise alter the pending request.
///
/// # Returns
///
/// Returns `true` when a scheduler checkpoint remains pending.
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) fn reschedule_pending() -> bool {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    state_mut().reschedule_pending
}

/// Return whether thread entry has made scheduler checkpoints available.
///
/// The query takes a short local-IRQ guard, allocates nothing, and is safe in
/// thread or IRQ context.
///
/// # Returns
///
/// Returns `true` after bootstrap has enabled scheduler checkpoints.
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) fn scheduler_online() -> bool {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    state_mut().scheduler_online
}

/// Mark the initialized scheduler available immediately before its first thread entry.
///
/// This is a bounded, allocation-free bootstrap operation. It requires no
/// active preemption exclusion and panics if called twice or while disabled.
///
/// # Returns
///
/// Returns after publishing the scheduler-checkpoint availability state.
///
/// # Panics
///
/// Panics when called more than once or with a nonzero preemption depth.
pub(crate) fn mark_scheduler_online() {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let state = state_mut();
    if state.scheduler_online {
        panic!("preempt: scheduler marked online twice");
    }
    if state.disable_depth != 0 {
        panic!("preempt: scheduler online while preemption disabled");
    }
    state.scheduler_online = true;
}

/// Consume one pending reschedule at a scheduler safe point.
///
/// Returns `true` only when the scheduler is online, preemption is enabled,
/// and this call acknowledged a pending request. The operation is bounded and
/// allocation-free; only scheduler checkpoints may clear the pending bit.
///
/// # Returns
///
/// Returns whether this call consumed the pending reschedule request.
pub(crate) fn checkpoint_consume() -> bool {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let state = state_mut();
    if !state.scheduler_online || state.disable_depth != 0 || !state.reschedule_pending {
        return false;
    }
    state.reschedule_pending = false;
    true
}

/// Fail before an operation that would block or make a terminal handoff.
///
/// This operation is bounded and allocation-free. `operation` names the exact
/// forbidden operation in the panic diagnostic.
///
/// # Arguments
///
/// * `operation` - Static name of the operation that requires thread preemption.
///
/// # Panics
///
/// Panics when a [`PreemptGuard`] is active.
pub(crate) fn assert_preemption_enabled(operation: &'static str) {
    if is_disabled() {
        panic!("preempt: {operation} while preemption disabled");
    }
}
