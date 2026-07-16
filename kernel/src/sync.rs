use core::{
    cell::UnsafeCell,
    marker::PhantomData,
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
/// than calling the architecture hooks directly. Guards may be nested because
/// each instance restores the exact IRQ state observed on entry. Construction
/// is bounded and allocation-free.
pub(crate) struct LocalIrqGuard {
    saved_daif: u64,
    _not_send: PhantomData<*mut ()>,
}

impl LocalIrqGuard {
    /// Save the current local IRQ state and mask IRQ delivery.
    ///
    /// # Returns
    ///
    /// Returns a non-copyable guard that restores the saved IRQ state when
    /// dropped. The operation is bounded, allocation-free, and does not alter
    /// scheduler policy.
    #[inline(always)]
    pub(crate) fn save_and_disable() -> Self {
        // SAFETY: the architecture layer returns the current local DAIF state and masks
        // IRQ delivery on this single core until `Drop` restores the saved state.
        let saved_daif = unsafe { arch_local_irq_save_and_disable() };
        Self {
            saved_daif,
            _not_send: PhantomData,
        }
    }
}

impl Drop for LocalIrqGuard {
    fn drop(&mut self) {
        // SAFETY: `saved_daif` came from `arch_local_irq_save_and_disable()` on the
        // same core, so restoring it here returns the caller to its prior IRQ state.
        unsafe { arch_local_irq_restore(self.saved_daif) }
    }
}

/// Local-IRQ exclusion lock for state shared with interrupt handlers.
///
/// In the current single-core kernel this masks local IRQs and treats recursive
/// or contended entry as a bug. It is not a spinlock and provides no SMP
/// synchronization guarantee. Locking is bounded and allocation-free.
pub(crate) struct LocalIrqLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

/// Exclusive borrow returned by [`LocalIrqLock::lock`].
///
/// Dropping the guard releases the lock before restoring the caller's saved
/// local IRQ state. The guard is intentionally neither `Copy` nor `Clone`.
pub(crate) struct LocalIrqLockGuard<'a, T> {
    owner: &'a LocalIrqLock<T>,
    _irq_guard: LocalIrqGuard,
}

// SAFETY: access to `value` is serialized by local IRQ exclusion and the
// recursive-entry flag on the active single core.
unsafe impl<T: Send> Sync for LocalIrqLock<T> {}

impl<T> LocalIrqLock<T> {
    /// Construct a local-IRQ exclusion lock.
    ///
    /// # Arguments
    ///
    /// * `value` - Initial value owned by the lock.
    ///
    /// # Returns
    ///
    /// Returns an unlocked container. Construction is constant-time,
    /// allocation-free, and does not alter IRQ state.
    pub(crate) const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Mask local IRQs and exclusively borrow the protected value.
    ///
    /// # Returns
    ///
    /// Returns a guard that restores the previous IRQ state when dropped. The
    /// operation is bounded and allocation-free.
    ///
    /// # Panics
    ///
    /// Panics on recursive or otherwise contended entry.
    pub(crate) fn lock(&self) -> LocalIrqLockGuard<'_, T> {
        let irq_guard = LocalIrqGuard::save_and_disable();
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            drop(irq_guard);
            panic!("sync: recursive or contended local IRQ lock entry");
        }

        LocalIrqLockGuard {
            owner: self,
            _irq_guard: irq_guard,
        }
    }
}

impl<T> Deref for LocalIrqLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the guard owns exclusive access while the lock is held.
        unsafe { &*self.owner.value.get() }
    }
}

impl<T> DerefMut for LocalIrqLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: the guard owns exclusive access while the lock is held.
        unsafe { &mut *self.owner.value.get() }
    }
}

impl<T> Drop for LocalIrqLockGuard<'_, T> {
    fn drop(&mut self) {
        self.owner.locked.store(false, Ordering::Release);
    }
}

/// Excludes task preemption until dropped.
///
/// This primitive is for state accessed during bootstrap or from task context
/// and must not be used by interrupt handlers. The current transitional backend
/// delegates to [`LocalIrqGuard`], so IRQs are masked while the guard exists. A
/// later deferred-reschedule implementation may replace that backend without
/// changing task-only lock ownership. Entry is bounded and allocation-free.
pub(crate) struct PreemptGuard {
    _irq_guard: LocalIrqGuard,
}

impl PreemptGuard {
    /// Enter a task-context preemption-excluded section.
    ///
    /// # Returns
    ///
    /// Returns a non-copyable guard that ends preemption exclusion when
    /// dropped. The current backend masks local IRQs; the operation is bounded
    /// and allocation-free.
    pub(crate) fn enter() -> Self {
        Self {
            _irq_guard: LocalIrqGuard::save_and_disable(),
        }
    }
}

/// Task-preemption exclusion lock for bootstrap and task-only mutable state.
///
/// This lock must not be acquired from interrupt context. Recursive or
/// contended entry is a bug. The current single-core implementation uses
/// [`PreemptGuard`], which temporarily masks local IRQs; it provides no SMP
/// synchronization guarantee. Locking is bounded and allocation-free.
pub(crate) struct PreemptLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

/// Exclusive borrow returned by [`PreemptLock::lock`].
///
/// Dropping the guard releases the value and ends task preemption exclusion.
/// The guard is intentionally neither `Copy` nor `Clone`.
pub(crate) struct PreemptLockGuard<'a, T> {
    owner: &'a PreemptLock<T>,
    _preempt_guard: PreemptGuard,
}

// SAFETY: task-context access to `value` is serialized by preemption exclusion
// and the recursive-entry flag on the active single core. IRQ callers are
// forbidden by the type's contract.
unsafe impl<T: Send> Sync for PreemptLock<T> {}

impl<T> PreemptLock<T> {
    /// Construct a task-preemption exclusion lock.
    ///
    /// # Arguments
    ///
    /// * `value` - Initial task-only value owned by the lock.
    ///
    /// # Returns
    ///
    /// Returns an unlocked container. Construction is constant-time,
    /// allocation-free, and does not alter IRQ state.
    pub(crate) const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Exclude task preemption and exclusively borrow the protected value.
    ///
    /// # Returns
    ///
    /// Returns a guard that ends preemption exclusion when dropped. The current
    /// backend masks local IRQs. The operation is bounded and allocation-free.
    ///
    /// # Panics
    ///
    /// Panics on recursive or otherwise contended entry.
    pub(crate) fn lock(&self) -> PreemptLockGuard<'_, T> {
        let preempt_guard = PreemptGuard::enter();
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            drop(preempt_guard);
            panic!("sync: recursive or contended preemption lock entry");
        }

        PreemptLockGuard {
            owner: self,
            _preempt_guard: preempt_guard,
        }
    }
}

impl<T> Deref for PreemptLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the guard owns exclusive task-context access while the lock
        // is held.
        unsafe { &*self.owner.value.get() }
    }
}

impl<T> DerefMut for PreemptLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: the guard owns exclusive task-context access while the lock
        // is held.
        unsafe { &mut *self.owner.value.get() }
    }
}

impl<T> Drop for PreemptLockGuard<'_, T> {
    fn drop(&mut self) {
        self.owner.locked.store(false, Ordering::Release);
    }
}
