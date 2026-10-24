//! Host-only architecture ABI stubs for pure kernel unit tests.
//!
//! Production links these names from the AArch64 architecture crate. The host
//! library tests exercise bounded scheduler tables without entering an
//! exception frame or touching hardware, so these stubs deliberately provide
//! only inert linkage semantics.

use core::{
    ffi::c_void,
    sync::atomic::{AtomicUsize, Ordering},
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingSystem;

std::thread_local! {
    static ALLOCATIONS: Cell<Option<(usize, usize, usize)>> = const { Cell::new(None) };
    static FAIL_ALLOCATION_SIZE: Cell<Option<usize>> = const { Cell::new(None) };
}

fn fail_allocation(size: usize) -> bool {
    FAIL_ALLOCATION_SIZE
        .try_with(|fail| {
            if fail.get() == Some(size) {
                fail.set(None);
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
}

fn count_allocation(index: usize) {
    let _ = ALLOCATIONS.try_with(|counts| {
        if let Some((mut alloc, mut realloc, mut dealloc)) = counts.get() {
            match index {
                0 => alloc += 1,
                1 => realloc += 1,
                _ => dealloc += 1,
            }
            counts.set(Some((alloc, realloc, dealloc)));
        }
    });
}

// SAFETY: every operation delegates the caller's unchanged allocation contract
// to System; the const-initialized TLS counters neither allocate nor recurse.
unsafe impl GlobalAlloc for CountingSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(0);
        if fail_allocation(layout.size()) {
            return core::ptr::null_mut();
        }
        // SAFETY: the caller supplies System's required valid layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation(0);
        if fail_allocation(layout.size()) {
            return core::ptr::null_mut();
        }
        // SAFETY: the caller supplies System's required valid layout.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation(1);
        if fail_allocation(size) {
            return core::ptr::null_mut();
        }
        // SAFETY: ptr/layout/size retain the caller's System allocation contract.
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count_allocation(2);
        // SAFETY: ptr/layout are the original System allocation pair.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static HOST_ALLOCATOR: CountingSystem = CountingSystem;

/// Count all heap operations on this host thread during a focused operation.
/// # Arguments
/// * `run` - Operation after fixture/open setup; other test threads are excluded.
/// # Returns
/// Operation result and `(alloc, realloc, dealloc)` counts. No allocator changes
/// affect production builds; counting itself does not allocate or mask IRQs.
/// # Panics
/// Panics for nested measurements; restores counting state if `run` panics.
pub(crate) fn allocations<R>(run: impl FnOnce() -> R) -> (R, (usize, usize, usize)) {
    struct Stop;
    impl Drop for Stop {
        fn drop(&mut self) {
            ALLOCATIONS.with(|counts| counts.set(None));
        }
    }
    ALLOCATIONS.with(|counts| {
        assert!(counts.get().is_none(), "nested allocation measurement");
        counts.set(Some((0, 0, 0)));
    });
    let stop = Stop;
    let result = run();
    let counts = ALLOCATIONS.with(|counts| counts.get().unwrap());
    drop(stop);
    (result, counts)
}

/// Fail one exact-sized System allocation/reallocation on this host thread.
/// # Arguments
/// * `size` - Requested byte count to reject once, after fixture setup.
/// * `run` - Fallible allocation path under test; must not use infallible allocation
///   for the selected size. Other host threads and production are unaffected.
/// # Returns
/// Operation result; restores injection state on return or unwind without allocation.
/// # Panics
/// Panics for nested injection or if no selected allocation was attempted.
pub(crate) fn failing_allocation<R>(size: usize, run: impl FnOnce() -> R) -> R {
    struct Stop;
    impl Drop for Stop {
        fn drop(&mut self) {
            FAIL_ALLOCATION_SIZE.with(|fail| fail.set(None));
        }
    }
    FAIL_ALLOCATION_SIZE.with(|fail| {
        assert!(fail.get().is_none(), "nested allocation failure injection");
        fail.set(Some(size));
    });
    let stop = Stop;
    let result = run();
    assert!(
        FAIL_ALLOCATION_SIZE.with(|fail| fail.get().is_none()),
        "allocation failure not exercised"
    );
    drop(stop);
    result
}

static CURRENT_CPU_LOGICAL_ID: AtomicUsize = AtomicUsize::new(0);

/// Publish one immutable synthetic CPU binding shared by profiler-aware fixtures.
/// Host scheduler models may invoke profiler hooks without booting a scheduler.
/// # Returns
/// Returns after one-time CPU0 registration; later calls preserve that binding.
/// # Panics
/// Panics if the synthetic boot CPU cannot be registered.
#[cfg(feature = "wake-profile")]
pub(crate) fn register_profile_test_cpu() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        crate::cpu::register_boot_cpu().expect("synthetic profiler boot CPU");
    });
}

std::thread_local! {
    static IRQ_MASK_STATE: Cell<usize> = const { Cell::new(0) };
    static TIMER_PROGRAM_COUNTS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    #[cfg(feature = "wake-profile")]
    static PROFILE_COUNTER_READS: Cell<u64> = const { Cell::new(0) };
}

pub(crate) fn irq_mask_state() -> usize {
    IRQ_MASK_STATE.with(Cell::get)
}

/// Return this host test thread's timer-arm and timer-disarm call counts.
///
/// # Returns
///
/// Returns `(arms, disarms)` for the inert architecture timer stubs. This query
/// does not allocate, block, or access hardware; counters are thread-local so
/// parallel host tests cannot affect each other's observations.
pub(crate) fn timer_program_counts() -> (usize, usize) {
    TIMER_PROGRAM_COUNTS.with(Cell::get)
}

#[unsafe(no_mangle)]
extern "C" fn arch_local_irq_save_and_disable() -> u64 {
    IRQ_MASK_STATE.with(|state| state.replace(1)) as u64
}
#[unsafe(no_mangle)]
extern "C" fn arch_local_irq_restore(saved: u64) {
    IRQ_MASK_STATE.with(|state| state.set(saved as usize));
}
#[unsafe(no_mangle)]
extern "C" fn arch_irq_state_allows_sched_call(_saved: u64) -> bool {
    true
}
#[unsafe(no_mangle)]
extern "C" fn arch_current_cpu_hardware_id() -> u64 {
    0
}
#[unsafe(no_mangle)]
extern "C" fn arch_bind_current_cpu_logical_id(logical_index: usize) -> bool {
    CURRENT_CPU_LOGICAL_ID.store(logical_index + 1, Ordering::Relaxed);
    true
}
#[unsafe(no_mangle)]
extern "C" fn arch_send_scheduler_ipi(_logical_index: usize) -> bool {
    true
}
#[unsafe(no_mangle)]
extern "C" fn arch_current_cpu_logical_id() -> usize {
    CURRENT_CPU_LOGICAL_ID.load(Ordering::Relaxed)
}
#[unsafe(no_mangle)]
extern "C" fn arch_expected_cpu_count() -> usize {
    1
}
#[unsafe(no_mangle)]
extern "C" fn arch_bootstrap_stack_capacity() -> usize {
    crate::config::KERNEL_CPU_CAPACITY
}
#[unsafe(no_mangle)]
extern "C" fn arch_secondary_cpu_identity_matches(_logical_index: usize) -> bool {
    true
}
#[unsafe(no_mangle)]
extern "C" fn arch_start_secondary_cpu(_logical_index: usize) -> i64 {
    -1
}
#[unsafe(no_mangle)]
extern "C" fn arch_system_restart() -> i64 {
    -1
}
#[unsafe(no_mangle)]
extern "C" fn arch_system_power_off() -> i64 {
    -1
}
#[unsafe(no_mangle)]
extern "C" fn arch_counter_now() -> u64 {
    0
}
#[unsafe(no_mangle)]
extern "C" fn arch_counter_freq_hz() -> u64 {
    1_000_000
}
#[cfg(feature = "wake-profile")]
#[unsafe(no_mangle)]
extern "C" fn arch_profile_counter() -> u64 {
    PROFILE_COUNTER_READS.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        n
    })
}
#[cfg(feature = "wake-profile")]
#[unsafe(no_mangle)]
extern "C" fn arch_profile_task_irq_enable() -> u64 {
    IRQ_MASK_STATE.with(|state| state.replace(0)) as u64
}

#[unsafe(no_mangle)]
extern "C" fn arch_task_irq_enable() -> u64 {
    IRQ_MASK_STATE.with(|state| state.replace(0)) as u64
}

/// Query ordered profiler reads on this host test thread, without side effects.
/// # Returns
/// Count of inert ordered reads; allocates nothing and touches no IRQ state.
#[cfg(feature = "wake-profile")]
pub(crate) fn profile_counter_reads() -> u64 {
    PROFILE_COUNTER_READS.with(Cell::get)
}
#[unsafe(no_mangle)]
extern "C" fn arch_timer_arm_deadline(_deadline: u64) {
    TIMER_PROGRAM_COUNTS.with(|counts| {
        let (arms, disarms) = counts.get();
        counts.set((arms + 1, disarms));
    });
}
#[unsafe(no_mangle)]
extern "C" fn arch_timer_disarm() {
    TIMER_PROGRAM_COUNTS.with(|counts| {
        let (arms, disarms) = counts.get();
        counts.set((arms, disarms + 1));
    });
}
#[unsafe(no_mangle)]
extern "C" fn arch_phys_to_virt(pa: usize) -> usize {
    pa
}
#[unsafe(no_mangle)]
extern "C" fn arch_query_user_mapping(
    _root_pa: usize,
    _va: usize,
    _out_info: *mut crate::memory::vm::UserMappingInfo,
) -> u64 {
    panic!("host test queried a hardware user mapping")
}
#[unsafe(no_mangle)]
extern "C" fn arch_current_user_page_readable(_va: usize) -> u64 {
    panic!("host test queried a live EL0 translation")
}
#[unsafe(no_mangle)]
extern "C" fn arch_sched_call(_request: *const c_void) {
    panic!("host test invoked private scheduler call")
}

#[unsafe(no_mangle)]
extern "C" fn arch_saved_context_init_kernel(
    _saved: *mut crate::arch::SavedContext,
    _stack_top: usize,
    _entry: usize,
    _arg: usize,
    _bootstrap: usize,
) {
}
#[unsafe(no_mangle)]
extern "C" fn arch_saved_context_init_user(
    _saved: *mut crate::arch::SavedContext,
    _entry: usize,
    _sp: usize,
    _kernel_sp: usize,
    _arg0: usize,
) {
}
#[unsafe(no_mangle)]
extern "C" fn arch_saved_context_init_fork_child(
    _saved: *mut crate::arch::SavedContext,
    _active: *const c_void,
    _kernel_sp: usize,
) {
}
#[unsafe(no_mangle)]
extern "C" fn arch_active_context_set_syscall_result(_frame: *mut c_void, _value: isize) {}
#[unsafe(no_mangle)]
extern "C" fn arch_active_context_restart_syscall(_frame: *mut c_void) {}
#[unsafe(no_mangle)]
extern "C" fn arch_active_context_replace_user(_frame: *mut c_void, _entry: usize, _sp: usize) {}
#[unsafe(no_mangle)]
extern "C" fn arch_saved_context_save(
    _saved: *mut crate::arch::SavedContext,
    _active: *const c_void,
) {
}
#[unsafe(no_mangle)]
extern "C" fn arch_saved_context_restore(
    _saved: *const crate::arch::SavedContext,
    _active: *mut c_void,
) {
}
#[unsafe(no_mangle)]
extern "C" fn arch_saved_context_enter(_saved: *const crate::arch::SavedContext) -> ! {
    panic!("host test entered saved context")
}
