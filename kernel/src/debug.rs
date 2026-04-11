#[cfg(debug_assertions)]
pub fn put_u64_dec(mut value: u64) {
    if value == 0 {
        crate::console::putc(b'0');
        return;
    }

    let mut buf = [0u8; 20];
    let mut idx = buf.len();
    while value != 0 {
        idx -= 1;
        buf[idx] = b'0' + (value % 10) as u8;
        value /= 10;
    }

    for &b in &buf[idx..] {
        crate::console::putc(b);
    }
}

#[cfg(debug_assertions)]
pub fn put_usize_dec(value: usize) {
    put_u64_dec(value as u64);
}
