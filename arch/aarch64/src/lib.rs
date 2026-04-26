#![no_std]

use core::arch::{asm, global_asm};

use bootinfo::BootInfo;

mod console;
mod esr;
mod exception;
mod gic;
mod mmio;
mod timer;
mod trap_frame;

use trap_frame::TrapFrame;

global_asm!(include_str!("boot.s"));
global_asm!(include_str!("exceptions.s"));

#[repr(align(8))]
struct AlignedBytes<const N: usize>([u8; N]);

// Keep the fallback DTB itself aligned in the kernel image so early boot code
// does not depend on byte-addressed parsing alone.
static EMBEDDED_DTB: AlignedBytes<{ include_bytes!(env!("GENRT_AARCH64_EMBEDDED_DTB")).len() }> =
    AlignedBytes(*include_bytes!(env!("GENRT_AARCH64_EMBEDDED_DTB")));

#[unsafe(no_mangle)]
pub static mut BOOT_CURRENT_EL: u64 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn rust_entry(dtb_pa: usize) -> ! {
    unsafe {
        gic::init_controller_minimal();
        gic::enable_irq(timer::TIMER_IRQ_ID_PHYS, 0x40);
        timer::early_init();
    }
    let dtb_pa = if dtb_pa != 0 {
        dtb_pa
    } else {
        let fallback = effective_dtb_pa(0);
        if fallback != 0 {
            kernel::debug!("arch: using embedded qemu DTB fallback");
        }
        fallback
    };
    let bootinfo: &'static BootInfo = unsafe { kernel::boot::init_bootinfo(dtb_pa) };
    kernel::kernel_main(bootinfo)
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_irq_enable() {
    // SAFETY: Called once the kernel is ready to receive timer IRQs.
    unsafe { timer::enable_cpu_irq() }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_local_irq_save_and_disable() -> u64 {
    let saved_daif: u64;
    unsafe {
        asm!(
            "mrs {saved_daif}, DAIF",
            "msr daifset, #2",
            "isb",
            saved_daif = out(reg) saved_daif,
            options(nomem, nostack, preserves_flags)
        );
    }
    saved_daif
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_local_irq_restore(saved_daif: u64) {
    unsafe {
        asm!(
            "msr DAIF, {saved_daif}",
            "isb",
            saved_daif = in(reg) saved_daif,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_counter_now() -> u64 {
    timer::counter()
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_counter_freq_hz() -> u64 {
    timer::frequency_hz()
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_timer_arm_deadline(deadline: u64) {
    // SAFETY: kernel passes an absolute architected-counter deadline.
    unsafe { timer::arm_deadline(deadline) }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_timer_disarm() {
    // SAFETY: kernel explicitly disables the timer when no deadlines are pending.
    unsafe { timer::disable() }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_task_call(request: *const core::ffi::c_void) {
    // SAFETY: `svc #0` raises a synchronous exception at the current EL. The
    // EL1 vector path saves the current TrapFrame and routes the request pointer
    // through `sync_entry()`. If the request blocks, execution resumes after this
    // instruction when the task is later woken.
    unsafe {
        asm!(
            "svc #0",
            in("x0") request,
            options(nostack)
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_init_task_frame(
    frame_words: *mut u64,
    stack_top: usize,
    entry_addr: usize,
    bootstrap_pc: usize,
) {
    if frame_words.is_null() {
        return;
    }

    // SAFETY: kernel passes valid task-owned frame storage matching TrapFrame ABI.
    let frame = unsafe { &mut *(frame_words as *mut TrapFrame) };
    *frame = TrapFrame::zeroed();
    frame.x[0] = entry_addr as u64;
    frame.sp = (stack_top as u64) & !0xF;
    frame.elr = bootstrap_pc as u64;
    frame.spsr = TrapFrame::EL1H;
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_hard_fault() -> ! {
    // SAFETY: this path is terminal by contract; IRQ/FIQ/SError are masked first.
    unsafe {
        asm!(
            "msr daifset, #0xf",
            options(nomem, nostack, preserves_flags)
        );
        asm!("isb", options(nomem, nostack, preserves_flags));
    }

    loop {
        // SAFETY: WFE loop is a deterministic hard-stop in early bring-up.
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

#[inline(always)]
fn effective_dtb_pa(dtb_pa: usize) -> usize {
    if dtb_pa != 0 {
        return dtb_pa;
    }

    if EMBEDDED_DTB.0.is_empty() {
        return 0;
    }

    EMBEDDED_DTB.0.as_ptr() as usize
}
