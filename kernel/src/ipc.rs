use alloc::{collections::VecDeque, vec::Vec};
use core::cell::UnsafeCell;

use crate::{
    arch::ActiveContext,
    sched::{self, CommitResult, CompletionResult, WaitCause, WaitKind, WaitToken},
    sync::{LocalIrqGuard, LocalIrqLock},
    task_call::TaskCallWaitOutput,
    time::TimedEvent,
};

const MAILBOX_WAIT_SEND: u64 = 1;
const MAILBOX_WAIT_RECV: u64 = 2;

#[derive(Debug, Eq, PartialEq)]
pub enum SendError<T> {
    Full(T),
}
#[derive(Debug, Eq, PartialEq)]
pub enum SendTimeoutError<T> {
    Timeout(T),
}
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecvError {
    Empty,
}
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecvTimeoutError {
    Timeout,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum MailboxWaitKind {
    Send,
    Recv,
}

impl MailboxWaitKind {
    fn from_raw(raw: u64) -> Option<Self> {
        match raw {
            MAILBOX_WAIT_SEND => Some(Self::Send),
            MAILBOX_WAIT_RECV => Some(Self::Recv),
            _ => None,
        }
    }
    const fn raw(self) -> u64 {
        match self {
            Self::Send => MAILBOX_WAIT_SEND,
            Self::Recv => MAILBOX_WAIT_RECV,
        }
    }
}

/// Bounded mailbox with exact-token waiter registrations.
///
/// The mailbox owns messages and wait queues. Scheduler state owns only each
/// wait token's lifecycle; no mailbox payload crosses that boundary. All
/// runtime storage is reserved at construction, and send/receive paths do not
/// allocate while the scheduler is active.
pub struct Mailbox<T> {
    control: LocalIrqLock<MailboxControl>,
    buffer: UnsafeCell<Vec<Option<T>>>,
}

// SAFETY: local IRQ exclusion serializes the control and generic buffer.
unsafe impl<T: Send> Sync for Mailbox<T> {}

impl<T> Mailbox<T> {
    pub fn with_capacity(capacity: usize, waiter_capacity: usize) -> Self {
        if capacity == 0 {
            panic!("ipc: mailbox capacity must be non-zero");
        }
        let mut buffer = Vec::new();
        buffer.reserve_exact(capacity);
        for _ in 0..capacity {
            buffer.push(None);
        }
        Self {
            control: LocalIrqLock::new(MailboxControl::with_capacity(capacity, waiter_capacity)),
            buffer: UnsafeCell::new(buffer),
        }
    }

    pub fn capacity(&self) -> usize {
        self.control.lock().capacity
    }
    pub fn len(&self) -> usize {
        self.control.lock().len
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn is_full(&self) -> bool {
        self.len() == self.capacity()
    }

    pub fn try_send(&self, msg: T) -> Result<(), SendError<T>> {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        let (len, waiter) = {
            let mut control = self.control.lock();
            if control.is_full() {
                return Err(SendError::Full(msg));
            }
            // SAFETY: `control` serializes the buffer indices and slots.
            let buffer = unsafe { &mut *self.buffer.get() };
            buffer[control.tail] = Some(msg);
            control.tail = (control.tail + 1) % control.capacity;
            control.len += 1;
            (control.len, control.claim_waiter(MailboxWaitKind::Recv))
        };
        self.complete_claimed_waiters(MailboxWaitKind::Recv, waiter);
        crate::trace!("ipc: send success len={len}");
        Ok(())
    }

    pub fn try_recv(&self) -> Result<T, RecvError> {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        let (msg, len, waiter) = {
            let mut control = self.control.lock();
            if control.is_empty() {
                return Err(RecvError::Empty);
            }
            // SAFETY: `control` serializes the buffer indices and slots.
            let buffer = unsafe { &mut *self.buffer.get() };
            let msg = buffer[control.head]
                .take()
                .unwrap_or_else(|| panic!("ipc: mailbox slot empty while len > 0"));
            control.head = (control.head + 1) % control.capacity;
            control.len -= 1;
            (
                msg,
                control.len,
                control.claim_waiter(MailboxWaitKind::Send),
            )
        };
        self.complete_claimed_waiters(MailboxWaitKind::Send, waiter);
        crate::trace!("ipc: recv success len={len}");
        Ok(msg)
    }

    pub fn send(&self, mut msg: T) {
        loop {
            match self.try_send(msg) {
                Ok(()) => return,
                Err(SendError::Full(value)) => {
                    msg = value;
                    self.wait(MailboxWaitKind::Send);
                }
            }
        }
    }

    pub fn recv(&self) -> T {
        loop {
            if let Ok(msg) = self.try_recv() {
                return msg;
            }
            self.wait(MailboxWaitKind::Recv);
        }
    }

    pub fn send_until_counter(&self, mut msg: T, deadline: u64) -> Result<(), SendTimeoutError<T>> {
        loop {
            match self.try_send(msg) {
                Ok(()) => return Ok(()),
                Err(SendError::Full(value)) => msg = value,
            }
            if deadline <= crate::time::now_counter() {
                return Err(SendTimeoutError::Timeout(msg));
            }
            if self.wait_until_counter(MailboxWaitKind::Send, deadline) == Some(WaitCause::Timeout)
            {
                return Err(SendTimeoutError::Timeout(msg));
            }
        }
    }

    pub fn recv_until_counter(&self, deadline: u64) -> Result<T, RecvTimeoutError> {
        loop {
            if let Ok(msg) = self.try_recv() {
                return Ok(msg);
            }
            if deadline <= crate::time::now_counter() {
                return Err(RecvTimeoutError::Timeout);
            }
            if self.wait_until_counter(MailboxWaitKind::Recv, deadline) == Some(WaitCause::Timeout)
            {
                return Err(RecvTimeoutError::Timeout);
            }
        }
    }

    pub fn send_timeout_ticks(
        &self,
        msg: T,
        timeout_ticks: u64,
    ) -> Result<(), SendTimeoutError<T>> {
        self.send_until_counter(msg, crate::time::now_counter().wrapping_add(timeout_ticks))
    }
    pub fn recv_timeout_ticks(&self, timeout_ticks: u64) -> Result<T, RecvTimeoutError> {
        self.recv_until_counter(crate::time::now_counter().wrapping_add(timeout_ticks))
    }
    pub fn send_timeout_ms(&self, msg: T, timeout_ms: u64) -> Result<(), SendTimeoutError<T>> {
        self.send_timeout_ticks(msg, crate::time::ms_to_counts(timeout_ms))
    }
    pub fn recv_timeout_ms(&self, timeout_ms: u64) -> Result<T, RecvTimeoutError> {
        self.recv_timeout_ticks(crate::time::ms_to_counts(timeout_ms))
    }
    pub fn send_timeout_us(&self, msg: T, timeout_us: u64) -> Result<(), SendTimeoutError<T>> {
        self.send_timeout_ticks(msg, crate::time::us_to_counts(timeout_us))
    }
    pub fn recv_timeout_us(&self, timeout_us: u64) -> Result<T, RecvTimeoutError> {
        self.recv_timeout_ticks(crate::time::us_to_counts(timeout_us))
    }

    fn wait(&self, kind: MailboxWaitKind) {
        if let Some(completion) = crate::task_call::mailbox_wait(self.control_ptr(), kind.raw()) {
            self.remove_waiter(kind, completion.token());
        }
    }

    fn wait_until_counter(&self, kind: MailboxWaitKind, deadline: u64) -> Option<WaitCause> {
        let completion =
            crate::task_call::mailbox_wait_until_counter(self.control_ptr(), kind.raw(), deadline)?;
        self.remove_waiter(kind, completion.token());
        Some(completion.cause())
    }

    fn control_ptr(&self) -> *const core::ffi::c_void {
        &self.control as *const LocalIrqLock<MailboxControl> as *const core::ffi::c_void
    }

    fn remove_waiter(&self, kind: MailboxWaitKind, token: WaitToken) -> bool {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        self.control.lock().remove_waiter(kind, token)
    }

    fn complete_claimed_waiters(&self, kind: MailboxWaitKind, mut token: Option<WaitToken>) {
        while let Some(wait) = token {
            crate::time::cancel_event(TimedEvent::WaitDeadline(wait));
            match sched::complete_wait(wait, WaitCause::Notified) {
                CompletionResult::WokeBlocked | CompletionResult::CompletedPrepared => return,
                CompletionResult::AlreadyCompleted | CompletionResult::Stale => {
                    token = self.control.lock().claim_waiter(kind);
                }
            }
        }
    }

    /// Return the total number of mailbox-owned wait registrations.
    ///
    /// This QEMU-test-only observation takes the mailbox owner lock, performs
    /// no allocation, and does not inspect scheduler wait metadata.
    ///
    /// # Returns
    ///
    /// Returns the bounded sum of send and receive waiter queue lengths.
    #[cfg(feature = "qemu-test-kernel-runtime")]
    pub(crate) fn waiter_count_for_test(&self) -> usize {
        let control = self.control.lock();
        control.recv_waiters.len() + control.send_waiters.len()
    }
}

struct MailboxControl {
    capacity: usize,
    head: usize,
    tail: usize,
    len: usize,
    recv_waiters: VecDeque<WaitToken>,
    send_waiters: VecDeque<WaitToken>,
}

impl MailboxControl {
    fn with_capacity(capacity: usize, waiter_capacity: usize) -> Self {
        let mut recv_waiters = VecDeque::new();
        recv_waiters.reserve_exact(waiter_capacity);
        let mut send_waiters = VecDeque::new();
        send_waiters.reserve_exact(waiter_capacity);
        Self {
            capacity,
            head: 0,
            tail: 0,
            len: 0,
            recv_waiters,
            send_waiters,
        }
    }
    fn is_empty(&self) -> bool {
        self.len == 0
    }
    fn is_full(&self) -> bool {
        self.len == self.capacity
    }
    fn should_wait(&self, kind: MailboxWaitKind) -> bool {
        match kind {
            MailboxWaitKind::Send => self.is_full(),
            MailboxWaitKind::Recv => self.is_empty(),
        }
    }
    fn enqueue_waiter(&mut self, kind: MailboxWaitKind, token: WaitToken) {
        let queue = self.waiters_mut(kind);
        if queue.iter().any(|queued| *queued == token) {
            panic!("ipc: wait token already queued");
        }
        if queue.len() == queue.capacity() {
            panic!("ipc: mailbox waiter queue capacity exhausted");
        }
        queue.push_back(token);
    }
    fn claim_waiter(&mut self, kind: MailboxWaitKind) -> Option<WaitToken> {
        self.waiters_mut(kind).pop_front()
    }
    fn remove_waiter(&mut self, kind: MailboxWaitKind, token: WaitToken) -> bool {
        let queue = self.waiters_mut(kind);
        let Some(index) = queue.iter().position(|queued| *queued == token) else {
            return false;
        };
        queue.remove(index);
        true
    }
    fn waiters_mut(&mut self, kind: MailboxWaitKind) -> &mut VecDeque<WaitToken> {
        match kind {
            MailboxWaitKind::Send => &mut self.send_waiters,
            MailboxWaitKind::Recv => &mut self.recv_waiters,
        }
    }
}

/// Publish and commit one mailbox wait from the private task-call path.
///
/// The mailbox lock covers condition check, scheduler preparation, queue token,
/// and optional deadline publication. It is dropped before commit, while the
/// controlled exception entry remains IRQ-masked. This allocation-free path
/// therefore has no lost-wakeup interval and never holds an owner lock over a
/// scheduler handoff.
///
/// # Arguments
///
/// * `context` - Exclusive live task-call context used if wait commit blocks.
/// * `control` - Opaque pointer to the mailbox's stable control lock.
/// * `raw_wait_kind` - Mailbox-private send/receive condition selector.
/// * `timeout_deadline` - Optional absolute architecture counter deadline for
///   the exact registration.
/// * `output` - Stack-owned task-call output retaining the exact token and an
///   optional completion observed before commit.
///
/// # Returns
///
/// Returns without registration if the condition is already available or the
/// deadline already expired. Otherwise returns after early completion or after
/// the blocked task resumes. The operation is bounded and allocation-free.
///
/// # Panics
///
/// Panics for a null or invalid control pointer, an unknown `raw_wait_kind`,
/// waiter/deadline capacity exhaustion, or a scheduler wait invariant failure.
///
/// # Safety
///
/// `control` must point to the live `LocalIrqLock<MailboxControl>` owned by the
/// calling mailbox and remain valid across a blocked task-call interval.
pub(crate) fn on_mailbox_wait_sync(
    context: &mut ActiveContext<'_>,
    control: *mut core::ffi::c_void,
    raw_wait_kind: u64,
    timeout_deadline: Option<u64>,
    output: &mut TaskCallWaitOutput,
) {
    if control.is_null() {
        panic!("ipc: mailbox wait with null control block");
    }
    let kind = MailboxWaitKind::from_raw(raw_wait_kind)
        .unwrap_or_else(|| panic!("ipc: invalid mailbox wait kind {raw_wait_kind}"));
    let _irq_guard = LocalIrqGuard::save_and_disable();
    // SAFETY: task-call users pass the stable control lock of their mailbox.
    let owner = unsafe { &*(control.cast::<LocalIrqLock<MailboxControl>>()) };
    let prepared = {
        let mut state = owner.lock();
        if !state.should_wait(kind) {
            return;
        }
        if timeout_deadline.is_some_and(|deadline| deadline <= crate::time::now_counter()) {
            return;
        }
        crate::sync::preempt::assert_preemption_enabled("mailbox waiter publication");
        let prepared = sched::prepare_wait(WaitKind::Ipc);
        let token = prepared.token();
        output.record_token(token);
        state.enqueue_waiter(kind, token);
        if let Some(deadline) = timeout_deadline {
            crate::time::schedule_event(deadline, TimedEvent::WaitDeadline(token));
        }
        prepared
    };
    match sched::commit_wait(context, prepared) {
        CommitResult::Blocked(_) => {}
        CommitResult::Early(cause) => output.record_early(cause),
        CommitResult::Stale => panic!("ipc: mailbox wait became stale before commit"),
    }
}
