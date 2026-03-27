#![no_std]

use bootinfo::BootInfo;

pub fn kernel_main(_boot: &'static BootInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
