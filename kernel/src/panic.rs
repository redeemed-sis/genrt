use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    crate::console::puts("[genrt] PANIC\r\n");

    if let Some(msg) = info.message().as_str() {
        crate::console::puts(msg);
        crate::console::puts("\r\n");
    }

    loop {
        core::hint::spin_loop();
    }
}
