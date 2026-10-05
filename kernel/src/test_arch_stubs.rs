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
use std::cell::Cell;

static CURRENT_CPU_LOGICAL_ID: AtomicUsize = AtomicUsize::new(0);
std::thread_local! {
    static IRQ_MASK_STATE: Cell<usize> = const { Cell::new(0) };
    static TIMER_PROGRAM_COUNTS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
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
