//! Finite task-context work on a borrowed live exception frame.

use super::preempt::PreemptGuard;

unsafe extern "C" {
    fn arch_task_irq_enable() -> u64;
    fn arch_local_irq_restore(saved_daif: u64);
}

/// Admits IRQ progress without allowing replacement of a borrowed live frame.
/// Never retain this guard over a scheduler blocking handoff or commit. Backend
/// submissions must return `Pending`, not sleep inside this window.
pub(crate) struct TaskWindow {
    saved: u64,
    _preempt: PreemptGuard,
}

impl TaskWindow {
    /// Enter finite, nonblocking task work with IRQs enabled.
    ///
    /// # Returns
    /// A CPU-bound guard restoring IRQ state before releasing preemption.
    /// Entry allocates nothing. Heap work and destruction are permitted inside.
    pub(crate) fn enter() -> Self {
        let preempt = PreemptGuard::enter();
        // SAFETY: task callers own their live frame; preemption exclusion
        // prevents IRQ return from handing that borrowed frame to a peer.
        let saved = unsafe { arch_task_irq_enable() };
        Self {
            saved,
            _preempt: preempt,
        }
    }
}

impl Drop for TaskWindow {
    fn drop(&mut self) {
        // SAFETY: paired opaque IRQ state on the guard's creating CPU.
        unsafe { arch_local_irq_restore(self.saved) };
    }
}
