use core::cell::UnsafeCell;

use crate::{
    arch::ActiveContext,
    sched::{self, CommitResult, WaitCause, WaitToken},
    sync::LocalIrqGuard,
};

#[cfg(not(test))]
unsafe extern "C" {
    fn arch_console_init_once();
    fn arch_console_putc_raw(c: u8);
}

// Host-only scheduler/unit tests link the generic kernel without the AArch64
// console object. These no-op ABI stubs keep diagnostics side-effect free.
#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn arch_console_init_once() {}

#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn arch_console_putc_raw(_c: u8) {}

#[inline]
fn console_init_once() {
    #[cfg(not(test))]
    // SAFETY: the selected architecture provides the console backend.
    unsafe {
        arch_console_init_once();
    }
    #[cfg(test)]
    arch_console_init_once();
}

#[inline]
fn console_putc_raw(c: u8) {
    #[cfg(not(test))]
    // SAFETY: the selected architecture provides the console backend.
    unsafe {
        arch_console_putc_raw(c);
    }
    #[cfg(test)]
    arch_console_putc_raw(c);
}

const STDIN_RX_CAPACITY: usize = 256;

struct StdinRx {
    ring: [u8; STDIN_RX_CAPACITY],
    head: usize,
    tail: usize,
    len: usize,
    overflow_count: usize,
    waiter: Option<WaitToken>,
    completed: Option<WaitToken>,
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
            completed: None,
        }
    }

    fn push(&mut self, byte: u8) -> Option<WaitToken> {
        if self.len == STDIN_RX_CAPACITY {
            // Drop-newest policy: keep already buffered input stable for the
            // blocked reader and count the overflow for diagnostics.
            self.overflow_count = self.overflow_count.saturating_add(1);
            return None;
        }

        self.ring[self.tail] = byte;
        self.tail = (self.tail + 1) % STDIN_RX_CAPACITY;
        self.len += 1;
        let token = self.waiter.take();
        if token.is_some() {
            self.completed = token;
        }
        token
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

    fn register_waiter(&mut self, waiter: WaitToken) -> bool {
        if self.completed.is_some() {
            return false;
        }
        match self.waiter {
            Some(existing) if existing == waiter => true,
            Some(_) => false,
            None => {
                self.waiter = Some(waiter);
                true
            }
        }
    }

    fn take_completed(&mut self, current: crate::sched::ThreadId) -> Option<WaitToken> {
        match self.completed {
            Some(token) if token.thread() == current => self.completed.take(),
            _ => None,
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
    console_init_once();

    if c == b'\n' {
        console_putc_raw(b'\r');
    }

    console_putc_raw(c);
}

pub fn puts(s: &str) {
    for b in s.bytes() {
        putc(b);
    }
}

pub fn on_stdin_byte(byte: u8) {
    let waiter = {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        let stdin = stdin_mut();
        stdin.push(byte)
    };

    if let Some(waiter) = waiter {
        let _ = sched::complete_wait(waiter, WaitCause::Notified);
    }
}

pub fn read_stdin(buffer: &mut [u8]) -> usize {
    if buffer.is_empty() {
        return 0;
    }

    let current = sched::current_thread_id();
    let (len, completed) = {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        let stdin = stdin_mut();
        let len = stdin.pop_into(buffer);
        let completed = current.and_then(|thread| stdin.take_completed(thread));
        (len, completed)
    };
    if let Some(token) = completed {
        match sched::finish_wait(token) {
            Ok(_) | Err(sched::FinishError::Stale) => {}
            Err(sched::FinishError::NotCompleted) => {
                panic!("stdin: consumed before wait completion")
            }
        }
    }
    len
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
    let prepared = {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        if !stdin_ref().is_empty() {
            return Ok(false);
        }

        crate::sync::preempt::assert_preemption_enabled("stdin waiter registration");
        let prepared = sched::prepare_wait();
        let token = prepared.token();
        if !stdin_mut().register_waiter(token) {
            let _ = sched::cancel_wait(prepared);
            return Err(());
        }
        prepared
    };

    // The architecture facade owns the instruction-width and resume-PC detail.
    // This call occurs before any bytes are copied, so retry is transparent.
    context.restart_current_syscall();
    match sched::commit_wait(context, prepared) {
        CommitResult::Blocked(_) | CommitResult::Early(_) => {}
        CommitResult::Stale => panic!("stdin: wait became stale before commit"),
    }
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
