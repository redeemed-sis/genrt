use core::cell::UnsafeCell;

use crate::{sched, sync::LocalIrqGuard, task::ThreadId};

unsafe extern "C" {
    fn arch_console_init_once();
    fn arch_console_putc_raw(c: u8);
    fn arch_restart_current_syscall(frame_words: *mut u64);
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

pub fn block_current_stdin_read_if_empty(active_frame_words: *mut u64) -> Result<bool, ()> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    if !stdin_ref().is_empty() {
        return Ok(false);
    }

    let current = sched::current_thread_id().ok_or(())?;
    if !stdin_mut().register_waiter(current) {
        return Err(());
    }

    // SAFETY: this is called only from lower-EL AArch64 `read(0)` syscall
    // before any bytes have been copied. AArch64 SVC is one fixed 4-byte
    // instruction, so rewinding ELR makes the syscall transparent after wake.
    unsafe { arch_restart_current_syscall(active_frame_words) };
    sched::block_current_on_stdin_read(active_frame_words);
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
