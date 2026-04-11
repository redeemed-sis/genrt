use core::sync::atomic::{AtomicU64, Ordering};

static TICKS: AtomicU64 = AtomicU64::new(0);

#[cfg(debug_assertions)]
const TICK_LOG_EVERY: u64 = 100;

#[inline(always)]
pub fn on_tick_interrupt() {
    let n = TICKS.fetch_add(1, Ordering::Relaxed) + 1;

    #[cfg(debug_assertions)]
    if n.is_multiple_of(TICK_LOG_EVERY) {
        log_tick(n);
    }
}

#[inline(always)]
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

#[cfg(debug_assertions)]
fn log_tick(n: u64) {
    crate::console::puts("[tick] n=");
    put_u64_dec(n);
    crate::console::puts("\r\n");
}

#[cfg(debug_assertions)]
fn put_u64_dec(mut value: u64) {
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
