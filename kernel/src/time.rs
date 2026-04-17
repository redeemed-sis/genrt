use core::sync::atomic::{AtomicU64, Ordering};

static TICKS: AtomicU64 = AtomicU64::new(0);

#[cfg(debug_assertions)]
const TICK_LOG_EVERY: u64 = 100;

#[inline(always)]
pub fn on_tick_interrupt() -> u64 {
    let n = TICKS.fetch_add(1, Ordering::Relaxed) + 1;

    #[cfg(debug_assertions)]
    if n.is_multiple_of(TICK_LOG_EVERY) {
        log_tick(n);
    }

    n
}

#[inline(always)]
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

#[cfg(debug_assertions)]
fn log_tick(n: u64) {
    crate::debug!("tick={n}");
}
