//! Versioned records emitted only by test-enabled kernels.

use core::{
    fmt::{self, Write},
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::sync::LocalIrqGuard;

#[used]
#[unsafe(link_section = ".genrt.test_marker")]
static TEST_ARTIFACT_MARKER: [u8; 22] = *b"GENRT_TEST_ARTIFACT_V1";

static SEQUENCE: AtomicUsize = AtomicUsize::new(1);
const RECORD_CAPACITY: usize = 256;

struct RecordBuffer {
    bytes: [u8; RECORD_CAPACITY],
    len: usize,
}

impl RecordBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; RECORD_CAPACITY],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        // SAFETY: fmt::Write accepts UTF-8 `str` inputs and only copies their
        // bytes into this buffer, so the initialized prefix remains UTF-8.
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }
}

impl Write for RecordBuffer {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(fmt::Error)?;
        let destination = self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?;
        destination.copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

fn emit(event: &str, subject: &str, detail: Option<&str>) {
    // SAFETY: the marker is a valid static byte. The volatile read creates a
    // retained reference so the dedicated ELF section survives link GC.
    unsafe { core::ptr::read_volatile(TEST_ARTIFACT_MARKER.as_ptr()) };
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut record = RecordBuffer::new();
    let mut result = write!(record, "\x1eGTRT/1|kernel|{sequence:06}|{event}|{subject}");
    if let Some(detail) = detail {
        result = result.and_then(|()| write!(record, "|{detail}"));
    }
    result = result.and_then(|()| record.write_str("\n"));

    let _irq_guard = LocalIrqGuard::save_and_disable();
    if result.is_ok() {
        crate::console::puts(record.as_str());
    } else {
        crate::console::puts("\x1eGTRT/1|kernel|999999|ABORT|protocol|OVERFLOW\n");
    }
}

/// Announce that a kernel test coordinator reached its runnable boundary.
///
/// # Arguments
///
/// * `suite` - Stable machine-readable suite identifier.
///
/// # Returns
///
/// This function returns after writing one allocation-free UART record.
pub(crate) fn ready(suite: &str) {
    emit("READY", suite, None);
}

/// Announce the start of one kernel contract case.
///
/// # Arguments
///
/// * `case` - Stable machine-readable case identifier.
///
/// # Returns
///
/// This function returns after writing one allocation-free UART record.
pub(crate) fn case_start(case: &str) {
    emit("CASE_START", case, None);
}

/// Report successful completion of one kernel contract case.
///
/// # Arguments
///
/// * `case` - Stable machine-readable case identifier.
///
/// # Returns
///
/// This function returns after writing one allocation-free UART record.
pub(crate) fn pass(case: &str) {
    emit("PASS", case, None);
}

/// Report a failed kernel contract case and stop the test kernel.
///
/// # Arguments
///
/// * `case` - Stable machine-readable case identifier.
/// * `reason` - Stable enum-like failure code.
///
/// # Returns
///
/// This function never returns.
pub(crate) fn fail(case: &str, reason: &str) -> ! {
    emit("FAIL", case, Some(reason));
    loop {
        core::hint::spin_loop();
    }
}

/// Report successful completion of a kernel test suite.
///
/// # Arguments
///
/// * `suite` - Stable machine-readable suite identifier.
///
/// # Returns
///
/// This function never returns after emitting the terminal record.
pub(crate) fn done(suite: &str) -> ! {
    emit("DONE", suite, Some("PASS"));
    loop {
        core::hint::spin_loop();
    }
}

/// Report a test-infrastructure abort from a fatal kernel path.
///
/// # Arguments
///
/// * `subject` - Stable identifier for the failing subsystem.
/// * `reason` - Stable enum-like abort code.
///
/// # Returns
///
/// This function returns after writing the record so the normal fatal path can
/// retain control of final machine shutdown.
pub(crate) fn abort(subject: &str, reason: &str) {
    emit("ABORT", subject, Some(reason));
}
