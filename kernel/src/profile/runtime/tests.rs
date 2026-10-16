//! Per-CPU ownership and production IRQ/preemption window tests.
use super::*;
use crate::profile::format::FULL;

fn owner() -> ThreadId {
    ThreadId::new(7, 4)
}
fn cpu0() -> CpuId {
    CpuId::from_index(0).unwrap()
}
fn config(capacity: u32) -> Config {
    Config {
        mode: FULL,
        max_records: capacity,
        start_phase: 0,
        end_phase: 13,
    }
}
fn session(config: Config) -> Session {
    Session::allocate(owner(), cpu0(), config, |v, n| {
        v.try_reserve_exact(n).map_err(|_| errno::ENOMEM)
    })
    .unwrap()
}
#[test]
fn owner_lifecycle_conflicts_and_frozen_replacement_policy() {
    let mut state = CpuProfile::new();
    assert_eq!(state.start_allowed(owner()), Ok(()));
    state.session = Some(session(config(1)));
    assert_eq!(state.start_allowed(owner()), Err(errno::EBUSY));
    assert_eq!(state.start_allowed(ThreadId::new(7, 5)), Err(errno::EBUSY));
    assert!(matches!(
        state.take_owned(ThreadId::new(7, 5)),
        Err(errno::EPERM)
    ));
    assert!(state.detach_owner(ThreadId::new(7, 5)).is_none());
    let mut frozen = state.take_owned(owner()).unwrap();
    assert!(state.session.is_none());
    frozen.freeze();
    state.session = Some(frozen);
    assert_eq!(state.start_allowed(owner()), Ok(()));
    assert_eq!(state.start_allowed(ThreadId::new(8, 4)), Err(errno::EBUSY));
    // Removing ownership does not publish any inherited child state.
    let old = state.detach_owner(owner());
    assert!(state.session.is_none());
    drop(old);
    assert_eq!(state.start_allowed(ThreadId::new(8, 4)), Ok(()));
}

#[test]
fn task_irq_window_and_leaf_restore_their_exact_entry_states() {
    unsafe extern "C" {
        fn arch_local_irq_save_and_disable() -> u64;
        fn arch_local_irq_restore(saved: u64);
    }
    // Host architecture contract: task window admits IRQs, a leaf temporarily
    // masks them, and restoration recovers the original exception state.
    // Real PreemptGuard/synchronous-entry interaction remains a QEMU gate.
    let original = unsafe { arch_local_irq_save_and_disable() };
    let saved = unsafe { arch_profile_task_irq_enable() };
    assert_eq!(crate::test_arch_stubs::irq_mask_state(), 0);
    {
        let leaf = IrqSpinLock::new(CpuProfile::new());
        let mut locked = leaf.lock();
        assert_eq!(crate::test_arch_stubs::irq_mask_state(), 1);
        locked.session = None;
    }
    assert_eq!(crate::test_arch_stubs::irq_mask_state(), 0);
    let s = Session::allocate(owner(), cpu0(), config(1), |v, n| {
        assert_eq!(crate::test_arch_stubs::irq_mask_state(), 0);
        v.try_reserve_exact(n).map_err(|_| errno::ENOMEM)
    })
    .unwrap();
    drop(s);
    unsafe {
        arch_local_irq_restore(saved);
    }
    assert_eq!(crate::test_arch_stubs::irq_mask_state(), 1);
    unsafe {
        arch_local_irq_restore(original);
    }
}

#[test]
fn production_heap_window_excludes_preemption_and_restores_on_error() {
    // Transition fixtures share this immutable binding; only this test enters
    // global CPU-local preemption state rather than using local value models.
    crate::test_arch_stubs::register_profile_test_cpu();
    unsafe extern "C" {
        fn arch_local_irq_save_and_disable() -> u64;
    }
    let original = unsafe { arch_local_irq_save_and_disable() };
    for fail in [false, true] {
        {
            let _window = TaskHeapWindow::enter();
            assert!(crate::sync::preempt::is_disabled());
            assert_eq!(crate::test_arch_stubs::irq_mask_state(), 0);
            let allocated = Session::allocate(owner(), cpu0(), config(1), |v, n| {
                assert!(crate::sync::preempt::is_disabled());
                assert_eq!(crate::test_arch_stubs::irq_mask_state(), 0);
                if fail {
                    Err(errno::ENOMEM)
                } else {
                    v.try_reserve_exact(n).map_err(|_| errno::ENOMEM)
                }
            });
            assert_eq!(allocated.is_err(), fail);
            drop(allocated);
            assert!(crate::sync::preempt::is_disabled());
        }
        assert!(!crate::sync::preempt::is_disabled());
        assert_eq!(crate::test_arch_stubs::irq_mask_state(), 1);
    }
    unsafe {
        arch_local_irq_restore(original);
    }
}
