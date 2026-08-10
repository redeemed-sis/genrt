#[cfg(not(test))]
use core::panic::PanicInfo;

#[cfg(not(test))]
unsafe extern "C" {
    fn arch_hard_fault() -> !;
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    #[cfg(feature = "qemu-test")]
    crate::test_support::protocol::abort("kernel", "PANIC");
    crate::console::emergency_write(core::format_args!("[ERROR] panic: {info}\n"));

    // SAFETY: panic is terminal; architecture hard-fault path halts deterministically.
    unsafe { arch_hard_fault() }
}
