use core::arch::asm;

use crate::{esr, gic, timer, trap_frame::TrapFrame};

const VECTOR_CURRENT_EL_SPX_SYNC: u64 = 4;
const ISS_SVC_TASK_CALL: u32 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn irq_entry(frame: *mut TrapFrame) {
    // SAFETY: exception entry assembly always passes a valid trap frame pointer.
    let frame_words = frame as *mut u64;

    let iar = gic::acknowledge_irq();
    let irq_id = gic::irq_id_from_iar(iar);

    if !gic::is_spurious(irq_id) {
        if irq_id == timer::TIMER_IRQ_ID_PHYS {
            timer::on_timer_irq(frame_words);
        } else {
            kernel::warn!("irq: unexpected id=0x{irq_id:08x}");
        }
        gic::end_irq(iar);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn lower_el_irq_entry(frame: *mut TrapFrame) {
    // Lower-EL IRQ frames have the same GPR/ELR/SPSR shape as current-EL
    // frames, but `TrapFrame.sp` contains the saved user `SP_EL0` and
    // `TrapFrame.kernel_sp` contains the EL1 stack to reinstall on return.
    irq_entry(frame);
}

#[unsafe(no_mangle)]
pub extern "C" fn sync_entry(vector: u64, frame: *mut TrapFrame) {
    let raw_esr = read_esr_el1();
    let ec = esr::ec(raw_esr);
    let iss = esr::iss(raw_esr);

    // `sync_entry` is the narrow Rust-side dispatcher for controlled synchronous
    // traps that originate from kernel task code. We currently accept
    // scheduler-controlled `svc` calls from the current EL1 task path;
    // any other synchronous exception remains fatal and goes through
    // `exception_entry()` for diagnostics + halt.
    if vector == VECTOR_CURRENT_EL_SPX_SYNC && ec == esr::EC_SVC_AARCH64 && iss == ISS_SVC_TASK_CALL
    {
        // SAFETY: `exceptions.s` saved a live trap frame for the current EL1 task before
        // calling into Rust. `x0` carries a pointer to the typed task-call request
        // from `arch_task_call()`.
        let request = unsafe { (*frame).x[0] as *const core::ffi::c_void };
        kernel::task_call::on_arch_task_call(frame as *mut u64, request);
        return;
    }

    exception_entry(vector, frame as *const TrapFrame)
}

#[unsafe(no_mangle)]
pub extern "C" fn lower_el_sync_entry(vector: u64, frame: *mut TrapFrame) {
    let raw_esr = read_esr_el1();
    let ec = esr::ec(raw_esr);
    let iss = esr::iss(raw_esr);

    // This is the userspace syscall/fault boundary. It is deliberately separate
    // from `sync_entry()`, whose SVC #0 ABI is reserved for controlled EL1 task
    // calls such as sleep, mailbox wait, exit, and join.
    if ec == esr::EC_SVC_AARCH64 {
        kernel::syscall::dispatch(frame as *mut u64);
        return;
    }

    kernel::error!(
        "exception: lower EL sync fault EC=0x{ec:02x} ({}) ISS=0x{iss:08x}",
        esr::ec_name(ec)
    );
    exception_entry(vector, frame as *const TrapFrame)
}

#[unsafe(no_mangle)]
pub extern "C" fn exception_entry(vector: u64, frame: *const TrapFrame) -> ! {
    // SAFETY: fatal exception handling is terminal; masking interrupts makes
    // the diagnostic path deterministic even if a future caller forgets to.
    unsafe {
        asm!(
            "msr daifset, #0xf",
            options(nomem, nostack, preserves_flags)
        );
        asm!("isb", options(nomem, nostack, preserves_flags));
    }

    let (source, kind) = vector_source_kind(vector);
    let raw_esr = read_esr_el1();
    let raw_far = read_far_el1();
    let raw_current_el = read_current_el();
    let raw_spsr = read_spsr_el1();
    let ec = esr::ec(raw_esr);
    let iss = esr::iss(raw_esr);

    kernel::kprintln!();
    kernel::error!("exception: fatal");
    kernel::kprintln!("exception: source={source} kind={kind}");
    kernel::kprintln!(
        "exception: CurrentEL=0x{raw_current_el:016x} EL={}",
        (raw_current_el >> 2) & 0x3
    );
    kernel::kprintln!(
        "exception: ESR_EL1=0x{raw_esr:016x} EC=0x{ec:02x} ({}) ISS=0x{iss:08x}",
        esr::ec_name(ec)
    );
    kernel::kprintln!("exception: FAR_EL1=0x{raw_far:016x}");
    kernel::kprintln!("exception: ELR_EL1=0x{:016x}", read_elr_el1());
    kernel::kprintln!("exception: SPSR_EL1=0x{raw_spsr:016x}");

    if frame.is_null() {
        kernel::kprintln!("exception: trap_frame=<null>");
    } else {
        // SAFETY: exception entry assembly passes a live saved trap frame.
        let tf = unsafe { &*frame };
        kernel::kprintln!(
            "exception: tf.x0=0x{:016x} tf.x1=0x{:016x} tf.x2=0x{:016x} tf.x3=0x{:016x}",
            tf.x[0],
            tf.x[1],
            tf.x[2],
            tf.x[3]
        );
        kernel::kprintln!(
            "exception: tf.x29=0x{:016x} tf.x30=0x{:016x}",
            tf.x[29],
            tf.x[30]
        );
        kernel::kprintln!(
            "exception: tf.mode={:?} tf.sp=0x{:016x} tf.kernel_sp=0x{:016x}",
            tf.frame_mode(),
            tf.sp,
            tf.kernel_sp
        );
        kernel::kprintln!(
            "exception: tf.elr=0x{:016x} tf.spsr=0x{:016x}",
            tf.elr,
            tf.spsr
        );
        if is_lower_el_vector(vector) {
            kernel::kprintln!("exception: saved SP_EL0=0x{:016x}", tf.sp);
        }
    }

    crate::arch_hard_fault()
}

#[inline(always)]
fn is_lower_el_vector(vector: u64) -> bool {
    (8..=15).contains(&vector)
}

#[inline(always)]
fn vector_source_kind(vector: u64) -> (&'static str, &'static str) {
    match vector {
        0 => ("current_el_sp0", "sync"),
        1 => ("current_el_sp0", "irq"),
        2 => ("current_el_sp0", "fiq"),
        3 => ("current_el_sp0", "serror"),
        4 => ("current_el_spx", "sync"),
        5 => ("current_el_spx", "irq"),
        6 => ("current_el_spx", "fiq"),
        7 => ("current_el_spx", "serror"),
        8 => ("lower_el_aarch64", "sync"),
        9 => ("lower_el_aarch64", "irq"),
        10 => ("lower_el_aarch64", "fiq"),
        11 => ("lower_el_aarch64", "serror"),
        12 => ("lower_el_aarch32", "sync"),
        13 => ("lower_el_aarch32", "irq"),
        14 => ("lower_el_aarch32", "fiq"),
        15 => ("lower_el_aarch32", "serror"),
        _ => ("unknown", "unknown"),
    }
}

#[inline(always)]
fn read_esr_el1() -> u64 {
    let value: u64;
    // SAFETY: reading system register for exception diagnostics in EL1.
    unsafe {
        asm!(
            "mrs {value}, ESR_EL1",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
fn read_far_el1() -> u64 {
    let value: u64;
    // SAFETY: reading system register for exception diagnostics in EL1.
    unsafe {
        asm!(
            "mrs {value}, FAR_EL1",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
fn read_elr_el1() -> u64 {
    let value: u64;
    // SAFETY: reading system register for exception diagnostics in EL1.
    unsafe {
        asm!(
            "mrs {value}, ELR_EL1",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
fn read_spsr_el1() -> u64 {
    let value: u64;
    // SAFETY: reading system register for exception diagnostics in EL1.
    unsafe {
        asm!(
            "mrs {value}, SPSR_EL1",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
fn read_current_el() -> u64 {
    let value: u64;
    // SAFETY: reading current exception level for diagnostics.
    unsafe {
        asm!(
            "mrs {value}, CurrentEL",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}
