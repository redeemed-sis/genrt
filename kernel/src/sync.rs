use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

unsafe extern "C" {
    fn arch_local_irq_save_and_disable() -> u64;
    fn arch_local_irq_restore(saved_daif: u64);
}

/// Saves local IRQ state and masks IRQ delivery until dropped.
///
/// This is the current single-core critical-section primitive. It is deliberately
/// small and architecture-backed; higher-level locks should use this type rather
/// than calling the architecture hooks directly.
pub(crate) struct LocalIrqGuard {
    saved_daif: u64,
}

impl LocalIrqGuard {
    #[inline(always)]
    pub(crate) fn save_and_disable() -> Self {
        // SAFETY: the architecture layer returns the current local DAIF state and masks
        // IRQ delivery on this single core until `Drop` restores the saved state.
        let saved_daif = unsafe { arch_local_irq_save_and_disable() };
        Self { saved_daif }
    }
}

impl Drop for LocalIrqGuard {
    fn drop(&mut self) {
        // SAFETY: `saved_daif` came from `arch_local_irq_save_and_disable()` on the
        // same core, so restoring it here returns the caller to its prior IRQ state.
        unsafe { arch_local_irq_restore(self.saved_daif) }
    }
}

/// IRQ-save lock for short kernel critical sections.
///
/// In the current no-SMP kernel this masks local IRQs and treats lock contention
/// as a bug, which catches recursive/reentrant use. When SMP is introduced, this
/// type is the intended upgrade point for adding a real spin acquisition while
/// preserving the same IRQ-save API at call sites.
pub(crate) struct IrqSpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

pub(crate) struct IrqSpinLockGuard<'a, T> {
    owner: &'a IrqSpinLock<T>,
    _irq_guard: LocalIrqGuard,
}

// SAFETY: access to `value` is serialized by the IRQ-save lock.
unsafe impl<T: Send> Sync for IrqSpinLock<T> {}

impl<T> IrqSpinLock<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub(crate) fn lock(&self) -> IrqSpinLockGuard<'_, T> {
        let irq_guard = LocalIrqGuard::save_and_disable();
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            drop(irq_guard);
            panic!("sync: recursive or contended IRQ lock entry");
        }

        IrqSpinLockGuard {
            owner: self,
            _irq_guard: irq_guard,
        }
    }
}

impl<T> Deref for IrqSpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the guard owns exclusive access while the lock is held.
        unsafe { &*self.owner.value.get() }
    }
}

impl<T> DerefMut for IrqSpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: the guard owns exclusive access while the lock is held.
        unsafe { &mut *self.owner.value.get() }
    }
}

impl<T> Drop for IrqSpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.owner.locked.store(false, Ordering::Release);
    }
}
