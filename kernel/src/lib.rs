#![no_std]

pub mod boot;
pub mod console;
pub mod panic;
pub mod time;

use bootinfo::BootInfo;

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

    loop {
        core::hint::spin_loop();
    }
}
