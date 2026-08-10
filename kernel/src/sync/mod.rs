//! Allocation-free synchronization for shared kernel state.
//!
//! Local IRQ masking prevents re-entry on one CPU only.  Cross-CPU ownership
//! always uses [`SpinLock`] or [`IrqSpinLock`]; neither lock sleeps, yields, or
//! enters the scheduler while held.

use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) mod preempt;

use preempt::PreemptGuard;

unsafe extern "C" {
    fn arch_local_irq_save_and_disable() -> u64;
    fn arch_local_irq_restore(saved_daif: u64);
}

/// Saves local IRQ state and masks IRQ delivery until dropped.
///
/// This guard prevents local interrupt re-entry only; it is never an SMP
/// mutual-exclusion primitive. Construction is bounded and allocation-free.
pub(crate) struct LocalIrqGuard {
    saved_daif: u64,
    _not_send: PhantomData<*mut ()>,
}

impl LocalIrqGuard {
    /// Save the current local IRQ state and mask IRQ delivery.
    ///
    /// # Returns
    ///
    /// Returns a non-send guard that restores exactly the saved architecture
    /// state on drop. This is bounded, allocation-free, and does not block or
    /// enter the scheduler.
    #[inline(always)]
    pub(crate) fn save_and_disable() -> Self {
        // SAFETY: the architecture hook pairs this value with restore on the
        // current CPU.
        let saved_daif = unsafe { arch_local_irq_save_and_disable() };
        Self {
            saved_daif,
            _not_send: PhantomData,
        }
    }

    /// Return the opaque architecture IRQ state saved at entry.
    ///
    /// # Returns
    ///
    /// Returns the state without changing IRQ delivery, allocating, or
    /// blocking.
    #[inline(always)]
    pub(crate) fn saved_state(&self) -> u64 {
        self.saved_daif
    }
}

impl Drop for LocalIrqGuard {
    fn drop(&mut self) {
        // SAFETY: `saved_daif` was captured by the paired save hook on this
        // same CPU; the non-send marker prevents a safe cross-thread drop.
        unsafe { arch_local_irq_restore(self.saved_daif) }
    }
}

/// Non-fair raw spin primitive shared by both lock domains.
///
/// Successful acquisition is Acquire, retry polling is Relaxed, and release is
/// Release. It never allocates, sleeps, blocks the scheduler, or guarantees
/// fairness.
struct RawSpin {
    locked: AtomicBool,
}

impl RawSpin {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    fn lock(&self) {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
    }

    fn try_lock(&self) -> bool {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

/// SMP spin lock for thread/bootstrap state.
///
/// Acquiring this lock enters a `PreemptGuard` before spinning, so a holder is
/// never switched away in normal thread context. IRQ users must use
/// [`IrqSpinLock`] instead. Guards are non-send, non-fair, allocation-free,
/// and must not perform blocking, scheduler handoff, user copies, parsing, or
/// cleanup while held.
pub struct SpinLock<T> {
    raw: RawSpin,
    value: UnsafeCell<T>,
}

/// Exclusive borrow from [`SpinLock`].
pub struct SpinLockGuard<'a, T> {
    owner: &'a SpinLock<T>,
    preempt_guard: Option<ManuallyDrop<PreemptGuard>>,
    _not_send: PhantomData<*mut ()>,
}

// SAFETY: `RawSpin` serializes cross-CPU access; callers require `T: Send`.
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Construct an unlocked thread/bootstrap spin lock.
    ///
    /// # Arguments
    ///
    /// * `value` - Initial value owned by the lock.
    ///
    /// # Returns
    ///
    /// Returns an unlocked, allocation-free lock. Construction does not alter
    /// IRQ state or enter the scheduler.
    pub const fn new(value: T) -> Self {
        Self {
            raw: RawSpin::new(),
            value: UnsafeCell::new(value),
        }
    }

    /// Acquire exclusive cross-CPU access.
    ///
    /// # Returns
    ///
    /// Returns a non-send guard after an Acquire acquisition. The operation
    /// spins without allocation or scheduler blocking and leaves IRQ state
    /// unchanged.
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        let preempt_guard = enter_preempt_guard();
        self.raw.lock();
        SpinLockGuard {
            owner: self,
            preempt_guard,
            _not_send: PhantomData,
        }
    }

    /// Attempt exclusive access without spinning.
    ///
    /// # Returns
    ///
    /// Returns `Some(guard)` on Acquire success and `None` when another CPU
    /// owns the lock. It allocates nothing and is suitable for panic/emergency
    /// fallbacks; it leaves IRQ state unchanged.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        let preempt_guard = enter_preempt_guard();
        if !self.raw.try_lock() {
            drop(preempt_guard);
            return None;
        }
        Some(SpinLockGuard {
            owner: self,
            preempt_guard,
            _not_send: PhantomData,
        })
    }
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.owner.value.get() }
    }
}
impl<T> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.owner.value.get() }
    }
}
impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.owner.raw.unlock();
        // SAFETY: this guard uniquely owns its preemption guard. Releasing the
        // raw lock precedes a possible scheduler checkpoint on guard drop.
        if let Some(preempt_guard) = self.preempt_guard.as_mut() {
            // SAFETY: this guard uniquely owns the nested preemption guard.
            unsafe { ManuallyDrop::drop(preempt_guard) };
        }
    }
}

#[cfg(not(test))]
fn enter_preempt_guard() -> Option<ManuallyDrop<PreemptGuard>> {
    Some(ManuallyDrop::new(PreemptGuard::enter()))
}

// Unit tests intentionally exercise raw cross-host-thread exclusion before a
// synthetic kernel CPU is registered. The production constructor always owns a
// PreemptGuard; host tests use OS threads that cannot safely share CPU-local
// kernel preemption bookkeeping.
#[cfg(test)]
fn enter_preempt_guard() -> Option<ManuallyDrop<PreemptGuard>> {
    None
}

/// SMP spin lock for state shared with local IRQ handlers.
///
/// Acquisition saves and disables local IRQs before spinning. Drop releases
/// the raw lock before restoring IRQ state. The guard is non-send and must not
/// block, schedule, allocate, copy userspace, parse, or perform heavy cleanup.
pub struct IrqSpinLock<T> {
    raw: RawSpin,
    value: UnsafeCell<T>,
}

/// Exclusive borrow from [`IrqSpinLock`].
pub struct IrqSpinLockGuard<'a, T> {
    owner: &'a IrqSpinLock<T>,
    irq_guard: ManuallyDrop<LocalIrqGuard>,
    _not_send: PhantomData<*mut ()>,
}

// SAFETY: the raw lock serializes CPUs and every guard masks local IRQs.
unsafe impl<T: Send> Sync for IrqSpinLock<T> {}

impl<T> IrqSpinLock<T> {
    /// Construct an unlocked IRQ-safe spin lock.
    ///
    /// # Arguments
    ///
    /// * `value` - Initial value owned by the lock.
    ///
    /// # Returns
    ///
    /// Returns an allocation-free lock. Construction does not change IRQ
    /// state, block, or enter the scheduler.
    pub const fn new(value: T) -> Self {
        Self {
            raw: RawSpin::new(),
            value: UnsafeCell::new(value),
        }
    }

    /// Mask local IRQs and acquire exclusive cross-CPU access.
    ///
    /// # Returns
    ///
    /// Returns a non-send guard after an Acquire acquisition. Retry polling is
    /// Relaxed and allocation-free; the operation never blocks the scheduler.
    pub fn lock(&self) -> IrqSpinLockGuard<'_, T> {
        let irq_guard = LocalIrqGuard::save_and_disable();
        self.raw.lock();
        IrqSpinLockGuard {
            owner: self,
            irq_guard: ManuallyDrop::new(irq_guard),
            _not_send: PhantomData,
        }
    }

    /// Try to acquire without spinning while IRQs are locally masked.
    ///
    /// # Returns
    ///
    /// Returns `Some(guard)` on Acquire success and `None` after restoring the
    /// prior IRQ state when contended. This is allocation-free and suitable for
    /// panic or logging fallbacks.
    pub fn try_lock(&self) -> Option<IrqSpinLockGuard<'_, T>> {
        let irq_guard = LocalIrqGuard::save_and_disable();
        if !self.raw.try_lock() {
            drop(irq_guard);
            return None;
        }
        Some(IrqSpinLockGuard {
            owner: self,
            irq_guard: ManuallyDrop::new(irq_guard),
            _not_send: PhantomData,
        })
    }
}

impl<T> Deref for IrqSpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.owner.value.get() }
    }
}
impl<T> DerefMut for IrqSpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.owner.value.get() }
    }
}
impl<T> Drop for IrqSpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.owner.raw.unlock();
        // SAFETY: restore occurs only after the Release unlock, so a newly
        // delivered local IRQ cannot observe protected state still locked.
        unsafe { ManuallyDrop::drop(&mut self.irq_guard) };
    }
}

/// One-shot Release publication for immutable-after-init state.
pub(crate) struct OncePublication<T> {
    init_lock: RawSpin,
    published: AtomicBool,
    value: UnsafeCell<core::mem::MaybeUninit<T>>,
}

// SAFETY: publication performs the only write before Release; reads occur
// only after Acquire observation and require `T: Sync`.
unsafe impl<T: Send + Sync> Sync for OncePublication<T> {}

impl<T> OncePublication<T> {
    /// Construct an unpublished storage cell.
    pub(crate) const fn new() -> Self {
        Self {
            init_lock: RawSpin::new(),
            published: AtomicBool::new(false),
            value: UnsafeCell::new(core::mem::MaybeUninit::uninit()),
        }
    }

    /// Publish an immutable value exactly once with Release ordering.
    ///
    /// # Arguments
    ///
    /// * `value` - Fully initialized value transferred into immutable storage.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` after publication or `Err(value)` when another value
    /// was already published. This does not allocate, block, or alter IRQ state.
    pub(crate) fn publish(&self, value: T) -> Result<(), T> {
        self.init_lock.lock();
        if self.published.load(Ordering::Relaxed) {
            self.init_lock.unlock();
            return Err(value);
        }
        // SAFETY: `init_lock` serializes publishers. The Release store below
        // makes initialization visible to readers that observe `true`.
        unsafe { (*self.value.get()).write(value) };
        self.published.store(true, Ordering::Release);
        self.init_lock.unlock();
        Ok(())
    }

    /// Acquire a shared immutable reference after successful publication.
    ///
    /// # Returns
    ///
    /// Returns `Some` only after an Acquire load observes publication; returns
    /// `None` before initialization. Reading is allocation-free and nonblocking.
    pub(crate) fn get(&self) -> Option<&T> {
        self.published
            .load(Ordering::Acquire)
            .then(|| unsafe { (&*self.value.get()).assume_init_ref() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Barrier},
        thread,
        vec::Vec,
    };

    #[test]
    fn spin_lock_serializes_host_threads() {
        let lock = Arc::new(SpinLock::new(0usize));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    for _ in 0..500 {
                        *lock.lock() += 1
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(*lock.lock(), 2_000);
    }

    #[test]
    fn publication_is_visible_after_acquire() {
        let publication = Arc::new(OncePublication::new());
        let barrier = Arc::new(Barrier::new(2));
        let publisher = Arc::clone(&publication);
        let publisher_barrier = Arc::clone(&barrier);
        let thread = thread::spawn(move || {
            publisher_barrier.wait();
            publisher.publish(42usize).unwrap();
        });
        barrier.wait();
        while publication.get().is_none() {
            core::hint::spin_loop();
        }
        thread.join().unwrap();
        assert_eq!(publication.get(), Some(&42));
        assert!(publication.publish(7).is_err());
    }

    #[test]
    fn irq_spin_lock_restores_saved_irq_state() {
        assert_eq!(crate::test_arch_stubs::irq_mask_state(), 0);
        {
            let lock = IrqSpinLock::new(7usize);
            let guard = lock.lock();
            assert_eq!(crate::test_arch_stubs::irq_mask_state(), 1);
            assert_eq!(*guard, 7);
        }
        assert_eq!(crate::test_arch_stubs::irq_mask_state(), 0);
    }
}
