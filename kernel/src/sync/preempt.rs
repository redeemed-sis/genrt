use core::{cell::UnsafeCell, marker::PhantomData};

use crate::{
    config::KERNEL_CPU_CAPACITY,
    cpu::{self, CpuId},
};

use super::LocalIrqGuard;

unsafe extern "C" {
    fn arch_irq_state_allows_sched_call(saved_irq_state: u64) -> bool;
}

/// CPU-local thread-preemption bookkeeping owned by one scheduler context.
///
/// Before scheduler publication, the same operations use separate fixed boot
/// backing so heap initialization remains available. Fields are mutated only
/// under short local-IRQ exclusion while CPU0 is the only active kernel
/// executor; this is not SMP synchronization.
pub(crate) struct PreemptionState {
    cpu: CpuId,
    disable_depth: usize,
    reschedule_pending: bool,
}

impl PreemptionState {
    /// Construct empty bookkeeping for one logical CPU.
    ///
    /// # Arguments
    ///
    /// * `cpu` - Logical CPU that permanently owns this state.
    ///
    /// # Returns
    ///
    /// Returns disabled-depth zero with no pending request. Construction is
    /// allocation-free and does not alter IRQ state.
    pub(crate) const fn new(cpu: CpuId) -> Self {
        Self {
            cpu,
            disable_depth: 0,
            reschedule_pending: false,
        }
    }

    /// Return the logical CPU that permanently owns this state.
    ///
    /// # Returns
    ///
    /// Returns the immutable owner without allocating, blocking, or changing
    /// IRQ state.
    pub(crate) const fn owner(&self) -> CpuId {
        self.cpu
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

// SAFETY: genrt's active target is single-core. Every access takes a short
// LocalIrqGuard, so thread and IRQ paths cannot concurrently mutate the state.
unsafe impl Sync for PreemptionCell {}

static BOOT_PREEMPTION: [PreemptionCell; KERNEL_CPU_CAPACITY] = [const {
    PreemptionCell(UnsafeCell::new(PreemptionState::new(
        CpuId::from_index(0).unwrap(),
    )))
}; KERNEL_CPU_CAPACITY];

#[inline(always)]
fn state_mut(cpu: CpuId) -> (&'static mut PreemptionState, bool) {
    if let Some(state) = crate::sched::runtime_preemption_state_mut(cpu) {
        return (state, true);
    }
    // SAFETY: callers hold LocalIrqGuard for the complete mutable access and
    // only CPU0 executes kernel code until the SMP synchronization milestone.
    let state = unsafe { &mut *BOOT_PREEMPTION[cpu.index()].0.get() };
    // Boot storage is initialized with CPU0 because stable const array
    // repetition cannot derive an index. Rewrite its owner before first use;
    // only the registered boot CPU is reachable in this milestone.
    state.cpu = cpu;
    (state, false)
}

#[inline(always)]
fn current_cpu() -> CpuId {
    cpu::current_id().unwrap_or_else(|err| panic!("preempt: current CPU lookup failed: {err:?}"))
}

/// Verify that fixed pre-scheduler backing can hand ownership to the scheduler.
///
/// Bootstrap calls this after all heap-backed scheduler storage has been built
/// and before publishing runtime CPU contexts. The check is bounded,
/// allocation-free, and uses short local IRQ exclusion.
///
/// # Arguments
///
/// * `cpu` - Registered boot CPU whose fixed backing is being retired.
///
/// # Returns
///
/// Returns when no guard or pending request remains in boot backing.
///
/// # Panics
///
/// Panics if a guard crosses scheduler publication or bootstrap leaves a
/// reschedule request without a valid checkpoint.
pub(crate) fn assert_boot_state_quiescent(cpu: CpuId) {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    // SAFETY: scheduler state is not yet published and the local IRQ guard
    // serializes access on the only active CPU.
    let state = unsafe { &mut *BOOT_PREEMPTION[cpu.index()].0.get() };
    if state.disable_depth != 0 || state.reschedule_pending {
        panic!("preempt: non-quiescent state at scheduler publication");
    }
}

/// Excludes thread preemption until dropped while preserving local IRQ state.
///
/// This primitive is for bootstrap or ordinary thread context only and must not
/// be used from an interrupt handler. Entry and drop mutate the private
/// single-core state under a short local-IRQ guard; neither operation allocates
/// nor blocks. An outermost drop may enter the private sched-call checkpoint.
pub(crate) struct PreemptGuard {
    cpu: CpuId,
    runtime_state: bool,
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
        let cpu = current_cpu();
        let _irq_guard = LocalIrqGuard::save_and_disable();
        let (state, runtime_state) = state_mut(cpu);
        state.enter();
        Self {
            cpu,
            runtime_state,
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
        let pending_at_depth_zero = {
            let (state, runtime_state) = state_mut(self.cpu);
            if runtime_state != self.runtime_state {
                panic!("preempt: guard crossed scheduler publication");
            }
            state.leave()
        };
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
    state_mut(cpu).0.is_disabled()
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
    state_mut(cpu).0.request();
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
    state_mut(cpu).0.pending()
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
    state_mut(cpu).0.assert_enabled(operation);
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
        let cpu0 = CpuId::from_index(0).unwrap();
        let cpu1 = CpuId::from_index(1).unwrap();
        let mut first = PreemptionState::new(cpu0);
        let mut second = PreemptionState::new(cpu1);

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
