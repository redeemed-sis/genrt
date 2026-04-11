#![no_std]

pub mod boot;
pub mod console;
pub mod panic;

use bootinfo::BootInfo;

unsafe extern "C" {
    fn arch_timer_irq_count() -> u64;
}

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(boot: &'static BootInfo) -> ! {
    console::puts("[genrt] kernel_main: entered\r\n");
    console::puts("[genrt] stage=phase1-week2\r\n");
    console::puts("[genrt] bootinfo:\r\n");
    console::puts("  arch=aarch64\r\n");

    if boot.dtb_pa != 0 {
        console::puts("  dtb=present\r\n");
    } else {
        console::puts("  dtb=absent\r\n");
    }

    let mut seen_timer_irq = false;
    loop {
        if !seen_timer_irq {
            // SAFETY: Provided by active architecture crate during final link.
            let irq_count = unsafe { arch_timer_irq_count() };
            if irq_count != 0 {
                seen_timer_irq = true;
                console::puts("[genrt] irq: first timer interrupt received\r\n");
            }
        }
        core::hint::spin_loop();
    }
}
