use core::{ffi::c_void, ptr::NonNull};

use kernel::arch::{ActiveContext, SavedContext, SyscallRequest};

use crate::trap_frame::TrapFrame;

const SYSCALL_NUMBER_REGISTER: usize = 8;
const SYSCALL_ARGUMENT_REGISTERS: core::ops::Range<usize> = 0..6;
const SVC_INSTRUCTION_BYTES: u64 = 4;

unsafe extern "C" {
    fn arch_enter_saved_context(frame: *const TrapFrame) -> !;
}

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

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_saved_context_init_kernel(
    saved: *mut SavedContext,
    stack_top: usize,
    entry_addr: usize,
    arg: usize,
    bootstrap_pc: usize,
) {
    // SAFETY: the kernel facade supplies exclusive SavedContext storage whose
    // size and alignment are checked below.
    let frame = unsafe { saved_frame_from_raw_mut(saved) };
    frame.init_kernel_el1(bootstrap_pc, stack_top, entry_addr, arg);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_saved_context_init_user(
    saved: *mut SavedContext,
    user_entry: usize,
    user_sp: usize,
    kernel_sp: usize,
    arg0: usize,
) {
    // SAFETY: the kernel facade supplies exclusive SavedContext storage whose
    // size and alignment are checked below.
    let frame = unsafe { saved_frame_from_raw_mut(saved) };
    frame.init_user_el0(user_entry, user_sp, kernel_sp, arg0);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_saved_context_init_fork_child(
    saved: *mut SavedContext,
    active: *const c_void,
    child_kernel_sp: usize,
) {
    // SAFETY: the facade supplies distinct destination storage and the
    // exclusively borrowed live parent frame.
    let dst = unsafe { saved_frame_from_raw_mut(saved) };
    let src = unsafe { active_frame_from_raw_const(active) };
    *dst = *src;
    dst.x[0] = 0;
    dst.kernel_sp = child_kernel_sp as u64 & !0xf;
}

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_saved_context_save(saved: *mut SavedContext, active: *const c_void) {
    // SAFETY: the facade supplies distinct, valid saved and live contexts.
    let dst = unsafe { saved_frame_from_raw_mut(saved) };
    let src = unsafe { active_frame_from_raw_const(active) };
    *dst = *src;
}

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_saved_context_restore(saved: *const SavedContext, active: *mut c_void) {
    // SAFETY: the facade supplies an initialized saved context and an exclusive
    // live return frame.
    let src = unsafe { saved_frame_from_raw(saved) };
    let dst = unsafe { active_frame_from_raw(active) };
    *dst = *src;
}

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_saved_context_enter(saved: *const SavedContext) -> ! {
    // SAFETY: the facade supplies stable initialized SavedContext storage. The
    // assembly routine reads the TrapFrame prefix and transfers control by eret.
    let frame = unsafe { saved_frame_from_raw(saved) };
    unsafe { arch_enter_saved_context(frame) }
}

unsafe fn active_frame_from_raw<'a>(frame: *mut c_void) -> &'a mut TrapFrame {
    let frame = NonNull::new(frame)
        .unwrap_or_else(|| panic!("arch: active context hook received null frame"));
    // SAFETY: only `kernel::arch::ActiveContext` invokes these hooks, and its
    // constructor contract guarantees a live, exclusive AArch64 TrapFrame.
    unsafe { &mut *frame.cast::<TrapFrame>().as_ptr() }
}

unsafe fn active_frame_from_raw_const<'a>(frame: *const c_void) -> &'a TrapFrame {
    let frame = NonNull::new(frame.cast_mut())
        .unwrap_or_else(|| panic!("arch: active context hook received null frame"));
    // SAFETY: SavedContext operations pass the live frame held exclusively by
    // ActiveContext for the duration of this call.
    unsafe { &*frame.cast::<TrapFrame>().as_ptr() }
}

unsafe fn saved_frame_from_raw<'a>(saved: *const SavedContext) -> &'a TrapFrame {
    let saved = NonNull::new(saved.cast_mut())
        .unwrap_or_else(|| panic!("arch: saved context hook received null storage"));
    // SAFETY: SavedContext is repr(C), its initialized byte storage starts at
    // offset zero, and the compile-time checks below guarantee fit/alignment.
    unsafe { &*saved.cast::<TrapFrame>().as_ptr() }
}

unsafe fn saved_frame_from_raw_mut<'a>(saved: *mut SavedContext) -> &'a mut TrapFrame {
    let saved = NonNull::new(saved)
        .unwrap_or_else(|| panic!("arch: saved context hook received null storage"));
    // SAFETY: the caller holds the unique mutable SavedContext borrow and the
    // compile-time checks below guarantee fit/alignment.
    unsafe { &mut *saved.cast::<TrapFrame>().as_ptr() }
}

const _: [(); SavedContext::PAYLOAD_BYTES] = [(); core::mem::size_of::<TrapFrame>()];
const _: [(); SavedContext::STORAGE_BYTES] = [(); core::mem::size_of::<SavedContext>()];
const _: [(); SavedContext::STORAGE_BYTES] = [(); TrapFrame::STACK_SIZE_BYTES];
const _: [(); SavedContext::STORAGE_ALIGN] = [(); core::mem::align_of::<SavedContext>()];
const _: () = assert!(core::mem::align_of::<SavedContext>() >= core::mem::align_of::<TrapFrame>());
