use core::{cell::UnsafeCell, marker::PhantomData};

use crate::{
    config::KERNEL_CPU_CAPACITY,
    cpu::{self, CpuId},
};

use super::LocalIrqGuard;

unsafe extern "C" {
    fn arch_irq_state_allows_sched_call(saved_irq_state: u64) -> bool;
}

/// CPU-local thread-preemption bookkeeping stored outside scheduler state.
///
/// The same fixed-capacity backing is available before heap initialization and
/// remains active after scheduler publication. Fields are mutated only by the
/// owning CPU under short local-IRQ exclusion; this is CPU-local ownership, not
/// cross-CPU synchronization.
pub(crate) struct PreemptionState {
    disable_depth: usize,
    reschedule_pending: bool,
}

impl PreemptionState {
    /// Construct empty CPU-local bookkeeping.
    ///
    /// # Returns
    ///
    /// Returns disabled-depth zero with no pending request. Construction is
    /// allocation-free and does not alter IRQ state.
    pub(crate) const fn new() -> Self {
        Self {
            disable_depth: 0,
            reschedule_pending: false,
        }
    }

    /// Increment this CPU's nested preemption-disable depth.
    ///
    /// # Returns
    ///
    /// Returns after a bounded, allocation-free counter update. The caller
    /// owns local IRQ exclusion.
    ///
    /// # Panics
    ///
    /// Panics if the nesting counter overflows.
    pub(crate) fn enter(&mut self) {
        self.disable_depth = self
            .disable_depth
            .checked_add(1)
            .unwrap_or_else(|| panic!("preempt: disable nesting overflow"));
    }

    /// Release one nested preemption-disable level.
    ///
    /// # Returns
    ///
    /// Returns `true` when the outermost level was released while a reschedule
    /// request remains pending. The caller owns local IRQ exclusion.
    ///
    /// # Panics
    ///
    /// Panics if no matching guard level exists.
    pub(crate) fn leave(&mut self) -> bool {
        self.disable_depth = self
            .disable_depth
            .checked_sub(1)
            .unwrap_or_else(|| panic!("preempt: disable nesting underflow"));
        self.disable_depth == 0 && self.reschedule_pending
    }

    /// Coalesce one reschedule request in this CPU's state.
    ///
    /// # Returns
    ///
    /// Returns after setting the pending bit. The bounded operation allocates
    /// nothing and requires caller-provided local IRQ exclusion.
    pub(crate) fn request(&mut self) {
        self.reschedule_pending = true;
    }

    /// Consume one pending request at a valid CPU-local checkpoint.
    ///
    /// # Arguments
    ///
    /// * `online` - Whether this state owner's scheduler context has entered
    ///   its first running thread.
    ///
    /// # Returns
    ///
    /// Returns `true` only when the CPU is online, preemption depth is zero,
    /// and a pending request was cleared. The operation is allocation-free.
    pub(crate) fn consume_checkpoint(&mut self, online: bool) -> bool {
        if !online || self.disable_depth != 0 || !self.reschedule_pending {
            return false;
        }
        self.reschedule_pending = false;
        true
    }

    /// Assert that a blocking or terminal transition may proceed.
    ///
    /// # Arguments
    ///
    /// * `operation` - Static operation name used in panic diagnostics.
    ///
    /// # Returns
    ///
    /// Returns when this CPU's preemption depth is zero.
    ///
    /// # Panics
    ///
    /// Panics while any `PreemptGuard` remains active on this CPU.
    pub(crate) fn assert_enabled(&self, operation: &'static str) {
        if self.disable_depth != 0 {
            panic!("preempt: {operation} while preemption disabled");
        }
    }

    #[cfg(any(test, feature = "qemu-test-kernel-runtime"))]
    const fn is_disabled(&self) -> bool {
        self.disable_depth != 0
    }

    #[cfg(any(test, feature = "qemu-test-kernel-runtime"))]
    const fn pending(&self) -> bool {
        self.reschedule_pending
    }
}

struct PreemptionCell(UnsafeCell<PreemptionState>);

// SAFETY: each cell is accessed only by its architecture-bound owning CPU.
// Every access also takes a short LocalIrqGuard, so owner thread and IRQ paths
// cannot concurrently mutate the same cell. Remote CPUs publish ready work and
// send a scheduler notification, but only the target's IPI handler mutates this
// CPU-local preemption bookkeeping.
unsafe impl Sync for PreemptionCell {}

static CPU_PREEMPTION: [PreemptionCell; KERNEL_CPU_CAPACITY] =
    [const { PreemptionCell(UnsafeCell::new(PreemptionState::new())) }; KERNEL_CPU_CAPACITY];

#[inline(always)]
fn with_state<R>(cpu: CpuId, f: impl FnOnce(&mut PreemptionState) -> R) -> R {
    if current_cpu() != cpu {
        panic!("preempt: remote CPU-local state access");
    }
    // SAFETY: this is CPU-local state indexed by the architecture-bound logical
    // CPU. Callers hold local IRQ exclusion, so neither local IRQ re-entry nor
    // another CPU can mutate this CPU's cell. Keeping it outside `Scheduler`
    // lets `SpinLock` enter PreemptGuard without taking the shared scheduler
    // lock.
    let state = unsafe { &mut *CPU_PREEMPTION[cpu.index()].0.get() };
    f(state)
}

#[inline(always)]
fn current_cpu() -> CpuId {
    cpu::current_id().unwrap_or_else(|err| panic!("preempt: current CPU lookup failed: {err:?}"))
}

/// Verify that CPU-local preemption state is quiescent before scheduler publication.
///
/// Bootstrap calls this after all heap-backed scheduler storage has been built
/// and before scheduler checkpoints become available. The fixed CPU-local
/// backing itself is retained for runtime. The check is bounded,
/// allocation-free, and uses short local IRQ exclusion.
///
/// # Arguments
///
/// * `cpu` - Registered boot CPU whose state must be quiescent.
///
/// # Returns
///
/// Returns when no guard or pending request remains before publication.
///
/// # Panics
///
/// Panics if bootstrap attempts publication with an active guard or a
/// reschedule request that has no valid checkpoint yet.
pub(crate) fn assert_pre_scheduler_state_quiescent(cpu: CpuId) {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    // SAFETY: scheduler state is not yet published and the local IRQ guard
    // serializes access on the only active CPU.
    let state = unsafe { &mut *CPU_PREEMPTION[cpu.index()].0.get() };
    if state.disable_depth != 0 || state.reschedule_pending {
        panic!("preempt: non-quiescent state at scheduler publication");
    }
}

/// Excludes thread preemption until dropped while preserving local IRQ state.
///
/// This primitive is for bootstrap or ordinary thread context only and must not
/// be used from an interrupt handler. Entry and drop mutate the executing CPU's
/// fixed local state under a short local-IRQ guard; neither operation allocates
/// nor blocks. An outermost drop may enter the private sched-call checkpoint.
pub(crate) struct PreemptGuard {
    cpu: CpuId,
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
    /// Panics when the current CPU's preemption nesting counter overflows.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn enter() -> Self {
        let cpu = current_cpu();
        let _irq_guard = LocalIrqGuard::save_and_disable();
        with_state(cpu, PreemptionState::enter);
        Self {
            cpu,
            _not_send: PhantomData,
        }
    }
}

impl Drop for PreemptGuard {
    fn drop(&mut self) {
        if current_cpu() != self.cpu {
            panic!("preempt: guard dropped on a different CPU");
        }
        let irq_guard = LocalIrqGuard::save_and_disable();
        let prior_irq_allows_sched_call = {
            // SAFETY: the saved IRQ state belongs to this guard on the current
            // core. The architecture hook owns DAIF encoding details.
            unsafe { arch_irq_state_allows_sched_call(irq_guard.saved_state()) }
        };
        let pending_at_depth_zero = with_state(self.cpu, PreemptionState::leave);
        let checkpoint = pending_at_depth_zero
            && crate::sched::scheduler_online(self.cpu)
            && prior_irq_allows_sched_call;
        drop(irq_guard);

        if checkpoint {
            // The checkpoint owns pending acknowledgement and can replace the
            // active thread context. Keep no live context or raw frame pointer in
            // the guard itself.
            crate::sched::call::preempt_checkpoint();
        }
    }
}

/// Return whether preemption is disabled on the executing CPU.
///
/// This test-only query takes a short local-IRQ guard, allocates nothing, and
/// does not acknowledge a pending reschedule request.
///
/// # Returns
///
/// Returns `true` when the current CPU has one or more active guards.
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) fn is_disabled() -> bool {
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    with_state(cpu, |state| state.is_disabled())
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
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    with_state(cpu, PreemptionState::request);
}

/// Request a checkpoint on one explicitly selected CPU-local state.
pub(crate) fn request_reschedule_on(cpu: CpuId) {
    // Remote publication sends a scheduler IPI instead of mutating another
    // CPU's local preemption state. The target-local IPI handler reaches this
    // function with `current_cpu() == cpu` after draining its ingress.
    if current_cpu() != cpu {
        return;
    }
    let _irq_guard = LocalIrqGuard::save_and_disable();
    with_state(cpu, PreemptionState::request);
}

/// Consume one pending checkpoint request for `cpu`.
pub(crate) fn consume_checkpoint_on(cpu: CpuId, online: bool) -> bool {
    if current_cpu() != cpu {
        panic!("preempt: remote checkpoint consumption");
    }
    let _irq_guard = LocalIrqGuard::save_and_disable();
    with_state(cpu, |state| state.consume_checkpoint(online))
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
    let cpu = current_cpu();
    let _irq_guard = LocalIrqGuard::save_and_disable();
    with_state(cpu, |state| state.pending())
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
    crate::sched::scheduler_online(current_cpu())
}

/// Verify that `cpu` has no active preemption exclusion.
///
/// This bounded operation takes short local IRQ exclusion and allocates or
/// blocks nothing.
///
/// # Arguments
///
/// * `cpu` - Logical CPU whose preemption depth is checked.
/// * `operation` - Static operation name used in panic diagnostics.
///
/// # Returns
///
/// Returns when the selected CPU's preemption depth is zero.
///
/// # Panics
///
/// Panics when the selected CPU has an active preemption guard.
pub(crate) fn assert_preemption_enabled_on(cpu: CpuId, operation: &'static str) {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    with_state(cpu, |state| state.assert_enabled(operation));
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
    let cpu = current_cpu();
    assert_preemption_enabled_on(cpu, operation);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preemption_state_is_cpu_local() {
        let mut first = PreemptionState::new();
        let mut second = PreemptionState::new();

        first.enter();
        first.request();

        assert!(first.is_disabled());
        assert!(first.pending());
        assert!(!second.is_disabled());
        assert!(!second.pending());
        assert!(!second.consume_checkpoint(true));
        assert!(first.leave());
        assert!(first.consume_checkpoint(true));
        assert!(!first.pending());
    }
}
