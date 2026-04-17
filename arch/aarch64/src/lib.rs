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

#[unsafe(no_mangle)]
pub static mut BOOT_CURRENT_EL: u64 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn rust_entry(dtb_pa: usize) -> ! {
    unsafe {
        gic::init_controller_minimal();
        gic::enable_irq(timer::TIMER_IRQ_ID_PHYS, 0x40);
        timer::early_init();
    }
    let bootinfo: &'static BootInfo = unsafe { kernel::boot::init_bootinfo(dtb_pa) };
    kernel::kernel_main(bootinfo)
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_irq_enable() {
    // SAFETY: Called once the kernel is ready to receive timer IRQs.
    unsafe { timer::enable_cpu_irq() }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_sleep_until(deadline: u64) {
    // SAFETY: `svc #0` raises a synchronous exception at the current EL.
    // The EL1 vector path saves the current TrapFrame, routes the request
    // through `sync_entry()`, lets the scheduler block this task, and
    // eventually resumes another task with `eret`. When the sleeping task is
    // woken later, execution continues after this instruction.
    unsafe {
        asm!(
            "svc #0",
            in("x0") deadline,
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
