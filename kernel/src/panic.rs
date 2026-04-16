use core::panic::PanicInfo;

unsafe extern "C" {
    fn arch_hard_fault() -> !;
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    crate::console::puts("[genrt] PANIC\r\n");

    if let Some(msg) = info.message().as_str() {
        crate::console::puts(msg);
        crate::console::puts("\r\n");
    }

    // SAFETY: panic is terminal; architecture hard-fault path halts deterministically.
    unsafe { arch_hard_fault() }
}
