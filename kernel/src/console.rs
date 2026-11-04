use crate::{
    config::KERNEL_THREAD_CAPACITY,
    errno::{self, Errno},
    fs::{
        backend::{Operation, Reply, Request},
        fd::FdTable,
        io::{Endpoint, IoObject},
    },
    sched::{self, WaitCause, WaitToken},
    sync::IrqSpinLock,
};
use alloc::sync::Arc;
use core::{
    fmt::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
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

/// Natural maximum currently available stdin data, not a usercopy byte limit.
pub(crate) const STDIN_RX_CAPACITY: usize = 256;
const CONSOLE_TX_CAPACITY: usize = 32 * 1024;
const CONSOLE_TX_DRAIN_CHUNK: usize = 64;
const CONSOLE_USER_TX_CHUNK: usize = 256;

struct StdinRx {
    ring: [u8; STDIN_RX_CAPACITY],
    head: usize,
    tail: usize,
    len: usize,
    overflow_count: usize,
    waiters: [Option<WaitToken>; KERNEL_THREAD_CAPACITY],
}

impl StdinRx {
    const fn new() -> Self {
        Self {
            ring: [0; STDIN_RX_CAPACITY],
            head: 0,
            tail: 0,
            len: 0,
            overflow_count: 0,
            waiters: [None; KERNEL_THREAD_CAPACITY],
        }
    }

    fn push(&mut self, byte: u8) -> [Option<WaitToken>; KERNEL_THREAD_CAPACITY] {
        if self.len == STDIN_RX_CAPACITY {
            // Drop-newest policy: keep already buffered input stable for the
            // blocked reader and count the overflow for diagnostics.
            self.overflow_count = self.overflow_count.saturating_add(1);
            return [None; KERNEL_THREAD_CAPACITY];
        }

        self.ring[self.tail] = byte;
        self.tail = (self.tail + 1) % STDIN_RX_CAPACITY;
        self.len += 1;
        core::mem::replace(&mut self.waiters, [None; KERNEL_THREAD_CAPACITY])
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

    fn poll_read(
        &mut self,
        buffer: &mut [u8],
        wake: Option<WaitToken>,
    ) -> Poll<Result<usize, Errno>> {
        if self.len != 0 || buffer.is_empty() {
            return Poll::Ready(Ok(self.pop_into(buffer)));
        }
        let Some(token) = wake else {
            return Poll::Ready(Err(errno::EAGAIN));
        };
        // Recheck and registration share the RX lock with the IRQ producer.
        self.waiters[token.thread().index()] = Some(token);
        Poll::Pending
    }

    fn cancel(&mut self, token: WaitToken) {
        if self.waiters[token.thread().index()] == Some(token) {
            self.waiters[token.thread().index()] = None;
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
    let _ = try_write_bytes(bytes);
}

/// Try to enqueue one complete console slice without dropping accepted bytes.
/// # Arguments
/// * `bytes` - Bounded message; larger than queue capacity is rejected intact.
/// # Returns
/// True only when every byte was enqueued. False means no byte was accepted;
/// syscall callers must return partial progress or `EAGAIN`, not full success.
/// Allocation-free; queue copying briefly masks IRQs, UART drain does not.
pub(crate) fn try_write_bytes(bytes: &[u8]) -> bool {
    console_init_once();
    let accepted = CONSOLE_TX.lock().push(bytes);
    if accepted {
        drain_console();
    }
    accepted
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

/// Publish one received UART byte and notify bounded exact readers.
/// # Arguments
/// * `byte` - Incoming byte; a full ring drops newest without allocation.
/// # Returns
/// Nothing; completes tokens only after releasing the RX lock. IRQ-safe.
pub fn on_stdin_byte(byte: u8) {
    let waiters = STDIN_RX.lock().push(byte);
    for waiter in waiters.into_iter().flatten() {
        let _ = sched::complete_wait(waiter, WaitCause::Notified);
    }
}

struct Uart;

impl IoObject for Uart {
    fn poll(&self, request: &mut Request, wake: Option<WaitToken>) -> Poll<Result<Reply, Errno>> {
        let count = match request.operation {
            Operation::Read { .. } => match STDIN_RX.lock().poll_read(&mut request.buffer, wake) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result?,
            },
            Operation::Write { .. } => {
                let mut accepted = 0;
                // Preserve large writes without extending any IRQ-masked copy.
                for chunk in request.buffer.chunks(CONSOLE_USER_TX_CHUNK) {
                    if !try_write_bytes(chunk) {
                        if accepted == 0 {
                            return Poll::Ready(Err(errno::EAGAIN));
                        }
                        break;
                    }
                    accepted += chunk.len();
                }
                accepted
            }
            _ => return Poll::Ready(Err(errno::EINVAL)),
        };
        Poll::Ready(Ok(Reply::Io { count, next: 0 }))
    }

    fn reset(&self, _request: &mut Request, wait: Option<WaitToken>) {
        if let Some(token) = wait {
            STDIN_RX.lock().cancel(token);
        }
    }

    fn transfer_len(&self, read: bool, requested: usize) -> usize {
        if read {
            requested.min(STDIN_RX_CAPACITY)
        } else {
            requested
        }
    }
}

/// Stage ordinary replaceable UART descriptors 0/1/2 before process publication.
/// # Returns
/// Table owning stdin (read), stdout and stderr (write); allocates reusable
/// requests in task context, without holding a process lock or blocking.
/// # Errors
/// `ENOMEM` if endpoint staging cannot be reserved.
pub(crate) fn stdio() -> Result<FdTable, Errno> {
    let object: Arc<dyn IoObject> = Arc::new(Uart);
    let mut table = FdTable::new();
    for fd in 0..3 {
        let mut descriptor = Some(Endpoint::descriptor(
            object.clone(),
            fd == 0,
            fd != 0,
            Request::new(Operation::Sync),
        )?);
        table
            .install(&mut descriptor)
            .expect("console: stdio slots");
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rx_registration_rechecks_data_wakes_bounded_readers_and_cancels_exactly() {
        let mut rx = StdinRx::new();
        let cpu = crate::cpu::CpuId::from_index(0).unwrap();
        let first = WaitToken::for_profile_test(sched::ThreadId::new(0, 1), cpu, 1);
        let later = WaitToken::for_profile_test(sched::ThreadId::new(0, 1), cpu, 2);
        let peer = WaitToken::for_profile_test(sched::ThreadId::new(1, 1), cpu, 3);
        let mut buffer = [0; 2];
        assert!(rx.poll_read(&mut buffer, Some(first)).is_pending());
        assert!(rx.poll_read(&mut buffer, Some(peer)).is_pending());
        let notified = rx.push(b'a'); // IRQ owns bytes, not a chosen consumer.
        assert_eq!(notified[0], Some(first));
        assert_eq!(notified[1], Some(peer));
        assert!(matches!(
            rx.poll_read(&mut buffer, Some(first)),
            Poll::Ready(Ok(1))
        ));
        assert_eq!(buffer[0], b'a');
        assert!(rx.poll_read(&mut buffer, Some(peer)).is_pending());
        rx.cancel(peer);
        assert_eq!(rx.waiters[1], None);
        assert!(rx.poll_read(&mut buffer, Some(later)).is_pending());
        rx.cancel(first); // Old operation cannot cancel the next exact wait.
        assert_eq!(rx.waiters[0], Some(later));
        assert_eq!(rx.push(b'b')[0], Some(later));
        rx.push(b'c'); // Data before registration never leads to Pending.
        assert!(matches!(
            rx.poll_read(&mut buffer, Some(first)),
            Poll::Ready(Ok(2))
        ));
        assert_eq!(&buffer, b"bc");
        assert!(rx.waiters.iter().all(Option::is_none));
        for _ in 0..STDIN_RX_CAPACITY {
            rx.push(b'x');
        }
        rx.push(b'y');
        assert_eq!(rx.overflow_count, 1);
        let mut full = [0; STDIN_RX_CAPACITY];
        assert_eq!(rx.pop_into(&mut full), STDIN_RX_CAPACITY);
        assert!(full.iter().all(|b| *b == b'x'));
        assert_eq!(Uart.transfer_len(false, usize::MAX), usize::MAX);
        assert_eq!(Uart.transfer_len(true, usize::MAX), STDIN_RX_CAPACITY);
    }

    #[test]
    fn saturated_tx_reports_rejection_without_consuming_any_chunk() {
        let mut queue = ConsoleTx::new();
        assert!(queue.push(&[b'a'; CONSOLE_TX_CAPACITY]));
        let tail = queue.tail;
        assert!(!queue.push(&[b'b'; 256]));
        assert_eq!(queue.len, CONSOLE_TX_CAPACITY);
        assert_eq!(queue.tail, tail);
        let mut chunk = [0; CONSOLE_TX_DRAIN_CHUNK];
        for _ in 0..4 {
            assert_eq!(queue.pop_chunk(&mut chunk), chunk.len());
            assert!(chunk.iter().all(|b| *b == b'a'));
        }
        assert!(queue.push(&[b'b'; 256]));
        let mut accepted = 256;
        if queue.push(&[b'c'; 256]) {
            accepted += 256;
        }
        assert_eq!(accepted, 256);
        let mut count_b = 0;
        while queue.pop_chunk(&mut chunk) != 0 {
            assert!(chunk.iter().all(|b| matches!(b, b'a' | b'b')));
            count_b += chunk.iter().filter(|b| **b == b'b').count();
        }
        assert_eq!(count_b, accepted);
    }
}
