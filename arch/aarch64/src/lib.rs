#![no_std]

use core::arch::global_asm;

use bootinfo::BootInfo;

mod console;
mod gic;
mod mmio;
mod timer;

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
pub extern "C" fn irq_entry() {
    let iar = gic::acknowledge_irq();
    let irq_id = gic::irq_id_from_iar(iar);

    if !gic::is_spurious(irq_id) {
        if irq_id == timer::TIMER_IRQ_ID_PHYS {
            timer::on_timer_irq();
        }
        gic::end_irq(iar);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_timer_irq_count() -> u64 {
    timer::irq_count()
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_entry(dtb_pa: usize) -> ! {
    unsafe {
        gic::init_controller_minimal();
        gic::enable_irq(timer::TIMER_IRQ_ID_PHYS, 0x40);
        timer::early_init();
        timer::enable_cpu_irq();
    }
    let bootinfo: &'static BootInfo = unsafe { kernel::boot::init_bootinfo(dtb_pa) };
    kernel::kernel_main(bootinfo)
}
