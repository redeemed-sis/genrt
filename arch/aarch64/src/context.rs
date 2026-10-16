use core::{arch::asm, ffi::c_void, ptr::NonNull};

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
    // SAFETY: parent live frame and child saved storage are distinct and valid.
    unsafe { copy_frame(dst, src) };
    dst.x[0] = 0;
    dst.kernel_sp = child_kernel_sp as u64 & !0xf;
}

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_saved_context_save(saved: *mut SavedContext, active: *const c_void) {
    // SAFETY: the facade supplies distinct, valid saved and live contexts.
    let dst = unsafe { saved_frame_from_raw_mut(saved) };
    let src = unsafe { active_frame_from_raw_const(active) };
    // SAFETY: the facade guarantees disjoint complete frame storage.
    unsafe { copy_frame(dst, src) };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn arch_saved_context_restore(saved: *const SavedContext, active: *mut c_void) {
    // SAFETY: the facade supplies an initialized saved context and an exclusive
    // live return frame.
    let src = unsafe { saved_frame_from_raw(saved) };
    let dst = unsafe { active_frame_from_raw(active) };
    // SAFETY: the facade guarantees disjoint complete frame storage.
    unsafe { copy_frame(dst, src) };
}

// Copy exactly the 280-byte payload, never the exception padding at +280.
// Fixed GP-register pairs avoid generic memmove/memcpy and FP/SIMD helpers.
// The explicit offsets are checked against the frame contract below.
#[inline(always)]
unsafe fn copy_frame(dst: &mut TrapFrame, src: &TrapFrame) {
    // SAFETY: callers provide distinct, initialized, u64-aligned frames.
    // All accesses stay in [0, 280); scratch registers cannot alias inputs.
    unsafe {
        asm!(
            ".irp offset, 0,16,32,48,64,80,96,112,128,144,160,176,192,208,224,240,256",
            "ldp {lo}, {hi}, [{src}, #\\offset]",
            "stp {lo}, {hi}, [{dst}, #\\offset]",
            ".endr",
            "ldr {lo}, [{src}, #272]",
            "str {lo}, [{dst}, #272]",
            src = in(reg) src as *const TrapFrame,
            dst = in(reg) dst as *mut TrapFrame,
            lo = out(reg) _,
            hi = out(reg) _,
            options(nostack, preserves_flags),
        );
    }
}

/// Verify the real saved/live frame copy with payload patterns and guards.
///
/// # Returns
///
/// Returns `true` when every payload word round-trips and the alignment tail,
/// exception padding, and surrounding guards remain unchanged. This bounded,
/// test-only probe uses stack storage, allocates nothing, does not block, and
/// does not alter IRQ state or enter the saved context.
#[cfg(feature = "qemu-test-kernel-runtime")]
#[unsafe(no_mangle)]
pub extern "C" fn arch_context_copy_for_test() -> bool {
    #[repr(C, align(16))]
    struct SavedProbe {
        before: [u64; 2],
        frame: SavedContext,
        after: [u64; 2],
    }
    #[repr(C, align(16))]
    struct LiveProbe {
        before: [u64; 2],
        frame: TrapFrame,
        padding: u64,
        after: [u64; 2],
    }
    let guard = [0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210];
    let mut live = LiveProbe {
        before: guard,
        frame: TrapFrame::zeroed(),
        padding: 0xa5a5_a5a5_a5a5_a5a5,
        after: guard,
    };
    for (i, register) in live.frame.x.iter_mut().enumerate() {
        *register = guard[i % 2] ^ (i as u64 + 1).wrapping_mul(0x1020_3040_5060_7080);
    }
    live.frame.sp = 0x1111_2222_3333_4444;
    live.frame.elr = 0x5555_6666_7777_8888;
    live.frame.spsr = 0x9999_aaaa_bbbb_cccc;
    live.frame.kernel_sp = 0xdddd_eeee_ffff_0000;
    let expected = live.frame;
    let mut saved = SavedProbe {
        before: guard,
        // SAFETY: SavedContext contains only initialized byte arrays; zero
        // storage is valid and is not entered as an execution context here.
        frame: unsafe { core::mem::zeroed() },
        after: guard,
    };
    // SAFETY: the probes contain disjoint initialized and aligned frames.
    unsafe {
        arch_saved_context_save(&mut saved.frame, (&raw const live.frame).cast());
        live.frame = TrapFrame::zeroed();
        arch_saved_context_restore(&saved.frame, (&raw mut live.frame).cast());
    }
    // SAFETY: the checked SavedContext layout includes these eight initialized
    // tail bytes after its payload. They must not be copied from a live frame.
    let tail = unsafe {
        ((&raw const saved.frame)
            .cast::<u8>()
            .add(SavedContext::PAYLOAD_BYTES) as *const u64)
            .read()
    };
    live.frame.x == expected.x
        && live.frame.sp == expected.sp
        && live.frame.elr == expected.elr
        && live.frame.spsr == expected.spsr
        && live.frame.kernel_sp == expected.kernel_sp
        && live.padding == 0xa5a5_a5a5_a5a5_a5a5
        && tail == 0
        && live.before == guard
        && live.after == guard
        && saved.before == guard
        && saved.after == guard
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
const _: () = assert!(TrapFrame::SIZE_BYTES == 280);
