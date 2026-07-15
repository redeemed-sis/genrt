use core::{ffi::c_void, ptr::NonNull};

use kernel::arch::{ActiveContext, SyscallRequest};

use crate::trap_frame::TrapFrame;

const SYSCALL_NUMBER_REGISTER: usize = 8;
const SYSCALL_ARGUMENT_REGISTERS: core::ops::Range<usize> = 0..6;
const SVC_INSTRUCTION_BYTES: u64 = 4;

/// Wrap the live AArch64 trap frame for generic kernel handling.
///
/// # Arguments
///
/// * `frame` - Exclusively borrowed frame saved by the active exception entry.
///
/// # Returns
///
/// Returns an opaque context with the same exclusive lifetime. Construction is
/// bounded and does not allocate, block, or alter IRQ state.
///
/// # Safety
///
/// `frame` must be the one live frame owned by the current exception entry, not
/// scheduler-saved storage, and no second context may be created for it.
pub(crate) unsafe fn active_context(frame: &mut TrapFrame) -> ActiveContext<'_> {
    let frame = NonNull::from(frame).cast::<c_void>();
    // SAFETY: upheld by this adapter's caller at the architecture entry point.
    unsafe { ActiveContext::from_raw(frame) }
}

/// Decode the AArch64 userspace syscall ABI from a saved trap frame.
///
/// # Arguments
///
/// * `frame` - Live lower-EL trap frame saved immediately after `svc`.
///
/// # Returns
///
/// Returns a request containing `x8` as the syscall number and `x0..x5` as
/// arguments. Decoding is bounded and does not allocate, block, or alter IRQ
/// state.
pub(crate) fn syscall_request(frame: &TrapFrame) -> SyscallRequest {
    let mut args = [0usize; 6];
    for (arg, register) in args.iter_mut().zip(SYSCALL_ARGUMENT_REGISTERS) {
        *arg = frame.x[register] as usize;
    }
    SyscallRequest::new(frame.x[SYSCALL_NUMBER_REGISTER] as usize, args)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_active_context_set_syscall_result(frame: *mut c_void, value: isize) {
    // SAFETY: the kernel facade passes its exclusively borrowed live frame.
    let frame = unsafe { active_frame_from_raw(frame) };
    frame.x[0] = value as u64;
}

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_active_context_restart_syscall(frame: *mut c_void) {
    // SAFETY: the kernel facade passes its exclusively borrowed live frame.
    let frame = unsafe { active_frame_from_raw(frame) };
    frame.elr = frame
        .elr
        .checked_sub(SVC_INSTRUCTION_BYTES)
        .unwrap_or_else(|| panic!("arch: cannot restart syscall at ELR=0"));
}

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_active_context_replace_user(
    frame: *mut c_void,
    user_entry: usize,
    user_sp: usize,
) {
    // SAFETY: the kernel facade passes its exclusively borrowed live frame.
    let frame = unsafe { active_frame_from_raw(frame) };
    let kernel_sp = frame.kernel_sp as usize;
    frame.init_user_el0(user_entry, user_sp, kernel_sp, 0);
}

unsafe fn active_frame_from_raw<'a>(frame: *mut c_void) -> &'a mut TrapFrame {
    let frame = NonNull::new(frame)
        .unwrap_or_else(|| panic!("arch: active context hook received null frame"));
    // SAFETY: only `kernel::arch::ActiveContext` invokes these hooks, and its
    // constructor contract guarantees a live, exclusive AArch64 TrapFrame.
    unsafe { &mut *frame.cast::<TrapFrame>().as_ptr() }
}
