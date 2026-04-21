use core::panic::PanicInfo;

unsafe extern "C" {
    fn arch_hard_fault() -> !;
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    crate::error!("panic: {info}");

    // SAFETY: panic is terminal; architecture hard-fault path halts deterministically.
    unsafe { arch_hard_fault() }
}
