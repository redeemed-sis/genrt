unsafe extern "C" {
    fn arch_console_init_once();
    fn arch_console_putc_raw(c: u8);
}

#[inline]
pub fn putc(c: u8) {
    // SAFETY: arch layer provides console backend for the selected target.
    unsafe {
        arch_console_init_once();
    }

    if c == b'\n' {
        // SAFETY: same as above.
        unsafe {
            arch_console_putc_raw(b'\r');
        }
    }

    // SAFETY: same as above.
    unsafe {
        arch_console_putc_raw(c);
    }
}

pub fn puts(s: &str) {
    for b in s.bytes() {
        putc(b);
    }
}
