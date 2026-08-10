use crate::{
    arch::ActiveContext,
    sched::{self, CommitResult, WaitCause, WaitToken},
    sync::{IrqSpinLock, LocalIrqGuard},
};
use core::{
    fmt::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
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
const CONSOLE_TX_CAPACITY: usize = 32 * 1024;
const CONSOLE_TX_DRAIN_CHUNK: usize = 64;

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

static STDIN_RX: IrqSpinLock<StdinRx> = IrqSpinLock::new(StdinRx::new());
static CONSOLE_TX: IrqSpinLock<ConsoleTx> = IrqSpinLock::new(ConsoleTx::new());
static CONSOLE_DRAINING: AtomicBool = AtomicBool::new(false);

struct ConsoleTx {
    ring: [u8; CONSOLE_TX_CAPACITY],
    head: usize,
    tail: usize,
    len: usize,
}

impl ConsoleTx {
    const fn new() -> Self {
        Self {
            ring: [0; CONSOLE_TX_CAPACITY],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() > CONSOLE_TX_CAPACITY - self.len {
            return false;
        }
        for byte in bytes {
            self.ring[self.tail] = *byte;
            self.tail = (self.tail + 1) % CONSOLE_TX_CAPACITY;
        }
        self.len += bytes.len();
        true
    }

    fn pop_chunk(&mut self, out: &mut [u8; CONSOLE_TX_DRAIN_CHUNK]) -> usize {
        let count = self.len.min(out.len());
        for destination in &mut out[..count] {
            *destination = self.ring[self.head];
            self.head = (self.head + 1) % CONSOLE_TX_CAPACITY;
        }
        self.len -= count;
        count
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[inline]
fn putc_raw(c: u8) {
    if c == b'\n' {
        console_putc_raw(b'\r');
    }

    console_putc_raw(c);
}

fn write_bytes_raw(bytes: &[u8]) {
    for byte in bytes {
        putc_raw(*byte);
    }
}

struct QueueWriter<'a> {
    queue: &'a mut ConsoleTx,
    accepted: bool,
}

impl Write for QueueWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.accepted && self.queue.push(value.as_bytes()) {
            Ok(())
        } else {
            self.accepted = false;
            Err(fmt::Error)
        }
    }
}

#[cfg(not(test))]
struct EmergencyWriter;

#[cfg(not(test))]
impl Write for EmergencyWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        write_bytes_raw(value.as_bytes());
        Ok(())
    }
}

fn drain_console() {
    if CONSOLE_DRAINING
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    loop {
        loop {
            let mut bytes = [0u8; CONSOLE_TX_DRAIN_CHUNK];
            let count = CONSOLE_TX.lock().pop_chunk(&mut bytes);
            if count == 0 {
                break;
            }
            write_bytes_raw(&bytes[..count]);
        }

        CONSOLE_DRAINING.store(false, Ordering::Release);
        if CONSOLE_TX.lock().is_empty()
            || CONSOLE_DRAINING
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
    }
}

#[inline]
pub fn putc(c: u8) {
    write_bytes(core::slice::from_ref(&c));
}

pub fn puts(s: &str) {
    write_bytes(s.as_bytes());
}

/// Write one byte slice as an indivisible normal console message.
///
/// # Arguments
///
/// * `bytes` - Bytes to write to the architecture console. Newline bytes are
///   expanded to the console's CRLF sequence by the single TX drainer.
///
/// # Returns
///
/// Returns after enqueueing the complete message and attempting synchronous
/// drain. The operation allocates nothing; local IRQs are masked only while
/// copying into the bounded queue, never while polling UART hardware. A full
/// queue drops the complete diagnostic message rather than interleaving it.
pub fn write_bytes(bytes: &[u8]) {
    console_init_once();
    if CONSOLE_TX.lock().push(bytes) {
        drain_console();
    }
}

/// Try to format one indivisible normal console message.
///
/// # Arguments
///
/// * `args` - Borrowed formatting arguments written without heap allocation.
///
/// # Returns
///
/// Returns `true` after atomically enqueueing the complete message, or `false`
/// when bounded queue capacity is unavailable. UART polling happens outside
/// the queue lock with local IRQ delivery restored.
pub(crate) fn try_write_fmt(args: fmt::Arguments<'_>) -> bool {
    console_init_once();
    let accepted = {
        let mut queue = CONSOLE_TX.lock();
        let original_tail = queue.tail;
        let original_len = queue.len;
        let mut writer = QueueWriter {
            queue: &mut queue,
            accepted: true,
        };
        let _ = writer.write_fmt(args);
        let accepted = writer.accepted;
        drop(writer);
        if !accepted {
            queue.tail = original_tail;
            queue.len = original_len;
        }
        accepted
    };
    if accepted {
        drain_console();
    }
    accepted
}

/// Write emergency diagnostics without acquiring the logging serializer.
///
/// This is the panic/re-entrant logging fallback. Output may interleave with a
/// normal message, but it never waits on the logging lock and never allocates.
#[cfg(not(test))]
pub(crate) fn emergency_write(args: fmt::Arguments<'_>) {
    console_init_once();
    let _ = EmergencyWriter.write_fmt(args);
}

pub fn on_stdin_byte(byte: u8) {
    let waiter = {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        let mut stdin = stdin_mut();
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
        let mut stdin = stdin_mut();
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
        let mut stdin = stdin_mut();
        if !stdin.is_empty() {
            return Ok(false);
        }

        crate::sync::preempt::assert_preemption_enabled("stdin waiter registration");
        let prepared = sched::prepare_wait();
        let token = prepared.token();
        if !stdin.register_waiter(token) {
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

fn stdin_mut() -> crate::sync::IrqSpinLockGuard<'static, StdinRx> {
    STDIN_RX.lock()
}
