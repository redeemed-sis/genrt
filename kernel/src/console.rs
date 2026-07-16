use core::cell::UnsafeCell;

use crate::{arch::ActiveContext, sched, sync::LocalIrqGuard, task::ThreadId};

unsafe extern "C" {
    fn arch_console_init_once();
    fn arch_console_putc_raw(c: u8);
}

const STDIN_RX_CAPACITY: usize = 256;

struct StdinRx {
    ring: [u8; STDIN_RX_CAPACITY],
    head: usize,
    tail: usize,
    len: usize,
    overflow_count: usize,
    waiter: Option<ThreadId>,
}

impl StdinRx {
    const fn new() -> Self {
        Self {
            ring: [0; STDIN_RX_CAPACITY],
            head: 0,
            tail: 0,
            len: 0,
            overflow_count: 0,
            waiter: None,
        }
    }

    fn push(&mut self, byte: u8) -> Option<ThreadId> {
        if self.len == STDIN_RX_CAPACITY {
            // Drop-newest policy: keep already buffered input stable for the
            // blocked reader and count the overflow for diagnostics.
            self.overflow_count = self.overflow_count.saturating_add(1);
            return None;
        }

        self.ring[self.tail] = byte;
        self.tail = (self.tail + 1) % STDIN_RX_CAPACITY;
        self.len += 1;
        self.waiter.take()
    }

    fn pop_into(&mut self, out: &mut [u8]) -> usize {
        let mut copied = 0usize;
        while copied < out.len() && self.len != 0 {
            out[copied] = self.ring[self.head];
            self.head = (self.head + 1) % STDIN_RX_CAPACITY;
            self.len -= 1;
            copied += 1;
        }
        copied
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn register_waiter(&mut self, waiter: ThreadId) -> bool {
        match self.waiter {
            Some(existing) if existing == waiter => true,
            Some(_) => false,
            None => {
                self.waiter = Some(waiter);
                true
            }
        }
    }
}

struct StdinCell(UnsafeCell<StdinRx>);

// SAFETY: stdin state is protected by local IRQ masking on the current
// single-core milestone. UART IRQ and syscall paths never access it concurrently
// with IRQs enabled.
unsafe impl Sync for StdinCell {}

static STDIN_RX: StdinCell = StdinCell(UnsafeCell::new(StdinRx::new()));

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

pub fn on_stdin_byte(byte: u8) {
    let waiter = {
        let stdin = stdin_mut();
        stdin.push(byte)
    };

    if let Some(waiter) = waiter {
        sched::complete_stdin_read(waiter);
    }
}

pub fn read_stdin(buffer: &mut [u8]) -> usize {
    if buffer.is_empty() {
        return 0;
    }

    let _irq_guard = LocalIrqGuard::save_and_disable();
    stdin_mut().pop_into(buffer)
}

/// Register and block the current stdin reader if the RX ring is still empty.
///
/// The lost-wakeup check, waiter registration, syscall restart, and scheduler
/// handoff occur under one short local IRQ-disabled section. The operation does
/// not allocate; when it blocks, control resumes through the scheduler-selected
/// live context.
///
/// # Arguments
///
/// * `context` - Exclusive live userspace syscall context to restart and hand
///   to the scheduler when input is unavailable.
///
/// # Returns
///
/// Returns `Ok(true)` after registering and blocking, `Ok(false)` when input
/// became available before registration, or `Err(())` when no current thread
/// exists or another stdin waiter owns the single waiter slot.
///
/// # Errors
///
/// Returns `Err(())` for unavailable scheduler identity or waiter-slot
/// conflict.
///
/// # Panics
///
/// Panics when a [`crate::sync::preempt::PreemptGuard`] is active before stdin waiter
/// registration.
pub(crate) fn block_current_stdin_read_if_empty(
    context: &mut ActiveContext<'_>,
) -> Result<bool, ()> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    if !stdin_ref().is_empty() {
        return Ok(false);
    }

    let current = sched::current_thread_id().ok_or(())?;
    crate::sync::preempt::assert_preemption_enabled("stdin waiter registration");
    if !stdin_mut().register_waiter(current) {
        return Err(());
    }

    // The architecture facade owns the instruction-width and resume-PC detail.
    // This call occurs before any bytes are copied, so retry is transparent.
    context.restart_current_syscall();
    sched::block_current_on_stdin_read(context);
    Ok(true)
}

fn stdin_mut() -> &'static mut StdinRx {
    // SAFETY: access discipline documented on `StdinCell`.
    unsafe { &mut *STDIN_RX.0.get() }
}

fn stdin_ref() -> &'static StdinRx {
    // SAFETY: access discipline documented on `StdinCell`.
    unsafe { &*STDIN_RX.0.get() }
}
