#![no_std]

use core::arch::global_asm;

use bootinfo::BootInfo;

mod console;
mod gic;
mod mmio;
mod timer;
mod trap_frame;

use trap_frame::TrapFrame;

global_asm!(include_str!("boot.s"));

#[unsafe(no_mangle)]
pub static mut BOOT_CURRENT_EL: u64 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn trap_record(_esr: u64, _far: u64, _elr: u64) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn irq_entry(frame: *mut TrapFrame) {
    // SAFETY: `boot.s` IRQ entry passes a valid pointer to a live trap frame.
    let frame_words = frame as *mut u64;

    let iar = gic::acknowledge_irq();
    let irq_id = gic::irq_id_from_iar(iar);

    if !gic::is_spurious(irq_id) {
        if irq_id == timer::TIMER_IRQ_ID_PHYS {
            timer::on_timer_irq(frame_words);
        }
        gic::end_irq(iar);
    }
}

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
