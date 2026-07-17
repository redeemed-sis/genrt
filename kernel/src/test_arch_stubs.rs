//! Host-only architecture ABI stubs for pure kernel unit tests.
//!
//! Production links these names from the AArch64 architecture crate. The host
//! library tests exercise bounded scheduler tables without entering an
//! exception frame or touching hardware, so these stubs deliberately provide
//! only inert linkage semantics.

use core::ffi::c_void;

#[unsafe(no_mangle)]
extern "C" fn arch_local_irq_save_and_disable() -> u64 {
    0
}
#[unsafe(no_mangle)]
extern "C" fn arch_local_irq_restore(_saved: u64) {}
#[unsafe(no_mangle)]
extern "C" fn arch_irq_state_allows_sched_call(_saved: u64) -> bool {
    true
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
extern "C" fn arch_timer_arm_deadline(_deadline: u64) {}
#[unsafe(no_mangle)]
extern "C" fn arch_timer_disarm() {}
#[unsafe(no_mangle)]
extern "C" fn arch_phys_to_virt(pa: usize) -> usize {
    pa
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
