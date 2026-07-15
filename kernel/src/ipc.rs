use alloc::{collections::VecDeque, vec::Vec};
use core::{cell::UnsafeCell, fmt};

use crate::{arch::ActiveContext, sync::IrqSpinLock, task::TaskId};

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

#[derive(Copy, Clone, Eq, PartialEq)]
enum IpcWaitObject {
    Mailbox(MailboxWaitKind),
}

/// Opaque scheduler token for an IPC wait.
///
/// The scheduler stores and returns this token but does not inspect the
/// concrete IPC primitive behind it. That keeps mailbox-specific cleanup in
/// `kernel::ipc` and leaves room for later semaphore/mutex/socket waiters to
/// reuse the same timeout path without teaching the scheduler about each one.
#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) struct IpcWaitToken {
    object: *mut core::ffi::c_void,
    wait_object: IpcWaitObject,
}

impl fmt::Debug for IpcWaitToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IpcWaitToken")
            .field("object", &self.object)
            .finish()
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct IpcWaitRegistration {
    token: IpcWaitToken,
    timeout_deadline: Option<u64>,
}

impl IpcWaitRegistration {
    fn mailbox(
        control: *mut core::ffi::c_void,
        wait_kind: MailboxWaitKind,
        timeout_deadline: Option<u64>,
    ) -> Self {
        Self {
            token: IpcWaitToken {
                object: control,
                wait_object: IpcWaitObject::Mailbox(wait_kind),
            },
            timeout_deadline,
        }
    }

    pub(crate) const fn token(self) -> IpcWaitToken {
        self.token
    }

    pub(crate) const fn timeout_deadline(self) -> Option<u64> {
        self.timeout_deadline
    }
}

/// Bounded mailbox for kernel-task IPC.
///
/// Blocking `send`/`recv` pass a pointer to this mailbox's control block through
/// the synchronous task-call path. The mailbox object must therefore remain
/// alive and must not be moved while any task may be waiting on it. Timeout
/// waits use time-owned `IpcTimeout(task)` events; normal mailbox wakeups cancel
/// those events before making the task runnable, while timeout dispatch removes
/// the task from this mailbox's bounded wait queue before waking it.
pub struct Mailbox<T> {
    control: IrqSpinLock<MailboxControl>,
    buffer: UnsafeCell<Vec<Option<T>>>,
}

// SAFETY: all mailbox state, including the message buffer, is protected by the
// non-generic IRQ-save control lock.
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
            control: IrqSpinLock::new(MailboxControl::with_capacity(capacity, waiter_capacity)),
            buffer: UnsafeCell::new(buffer),
        }
    }

    pub fn capacity(&self) -> usize {
        let control = self.control.lock();
        control.capacity()
    }

    pub fn len(&self) -> usize {
        let control = self.control.lock();
        control.len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_full(&self) -> bool {
        let control = self.control.lock();
        control.is_full()
    }

    pub fn try_send(&self, msg: T) -> Result<(), SendError<T>> {
        let (len, woken) = {
            let mut control = self.control.lock();
            if control.is_full() {
                return Err(SendError::Full(msg));
            }

            // SAFETY: `control` is the single lock protecting both the
            // non-generic queue indices and the generic message buffer.
            let buffer = unsafe { &mut *self.buffer.get() };
            buffer[control.tail] = Some(msg);
            control.tail = (control.tail + 1) % control.capacity();
            control.len += 1;

            let woken = control.pop_waiter(MailboxWaitKind::Recv);
            if let Some(task_id) = woken {
                crate::sched::complete_ipc_wait(task_id);
            }
            (control.len, woken)
        };

        crate::trace!("ipc: send success len={len}");
        if let Some(task_id) = woken {
            crate::trace!("ipc: woke recv waiter task {task_id}");
        }
        Ok(())
    }

    pub fn try_recv(&self) -> Result<T, RecvError> {
        let (msg, len, woken) = {
            let mut control = self.control.lock();
            if control.is_empty() {
                return Err(RecvError::Empty);
            }

            // SAFETY: `control` is the single lock protecting both the
            // non-generic queue indices and the generic message buffer.
            let buffer = unsafe { &mut *self.buffer.get() };
            let msg = buffer[control.head]
                .take()
                .unwrap_or_else(|| panic!("ipc: mailbox slot empty while len > 0"));
            control.head = (control.head + 1) % control.capacity();
            control.len -= 1;

            let woken = control.pop_waiter(MailboxWaitKind::Send);
            if let Some(task_id) = woken {
                crate::sched::complete_ipc_wait(task_id);
            }
            (msg, control.len, woken)
        };

        crate::trace!("ipc: recv success len={len}");
        if let Some(task_id) = woken {
            crate::trace!("ipc: woke send waiter task {task_id}");
        }
        Ok(msg)
    }

    pub fn send(&self, mut msg: T) {
        loop {
            match self.try_send(msg) {
                Ok(()) => return,
                Err(SendError::Full(returned)) => {
                    msg = returned;
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
                Err(SendError::Full(returned)) => msg = returned,
            }

            if deadline <= crate::time::now_counter() {
                crate::trace!("ipc: send_until_counter returned Timeout before blocking");
                return Err(SendTimeoutError::Timeout(msg));
            }

            if self.wait_until_counter(MailboxWaitKind::Send, deadline)
                == Some(crate::sched::WaitResult::TimedOut)
            {
                crate::debug!("ipc: send_until_counter returned Err(Timeout)");
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
                crate::trace!("ipc: recv_until_counter returned Timeout before blocking");
                return Err(RecvTimeoutError::Timeout);
            }

            if self.wait_until_counter(MailboxWaitKind::Recv, deadline)
                == Some(crate::sched::WaitResult::TimedOut)
            {
                crate::debug!("ipc: recv_until_counter returned Err(Timeout)");
                return Err(RecvTimeoutError::Timeout);
            }
        }
    }

    pub fn send_timeout_ticks(
        &self,
        msg: T,
        timeout_ticks: u64,
    ) -> Result<(), SendTimeoutError<T>> {
        let deadline = crate::time::now_counter().wrapping_add(timeout_ticks);
        self.send_until_counter(msg, deadline)
    }

    pub fn recv_timeout_ticks(&self, timeout_ticks: u64) -> Result<T, RecvTimeoutError> {
        let deadline = crate::time::now_counter().wrapping_add(timeout_ticks);
        self.recv_until_counter(deadline)
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

    fn wait(&self, wait_kind: MailboxWaitKind) {
        crate::task_call::mailbox_wait(
            &self.control as *const IrqSpinLock<MailboxControl> as *const core::ffi::c_void,
            wait_kind.raw(),
        );
    }

    fn wait_until_counter(
        &self,
        wait_kind: MailboxWaitKind,
        deadline: u64,
    ) -> Option<crate::sched::WaitResult> {
        crate::task_call::mailbox_wait_until_counter(
            &self.control as *const IrqSpinLock<MailboxControl> as *const core::ffi::c_void,
            wait_kind.raw(),
            deadline,
        );
        crate::sched::take_current_wait_result()
    }
}

struct MailboxControl {
    capacity: usize,
    head: usize,
    tail: usize,
    len: usize,
    recv_waiters: VecDeque<TaskId>,
    send_waiters: VecDeque<TaskId>,
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

    fn capacity(&self) -> usize {
        self.capacity
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

    fn enqueue_waiter(&mut self, kind: MailboxWaitKind, task_id: TaskId) {
        let waiters = self.waiters_mut(kind);
        if waiters.iter().any(|queued| *queued == task_id) {
            panic!("ipc: task {task_id} already queued on mailbox {kind:?} wait");
        }

        if waiters.len() == waiters.capacity() {
            panic!("ipc: mailbox waiter queue capacity exhausted");
        }

        waiters.push_back(task_id);
    }

    fn pop_waiter(&mut self, kind: MailboxWaitKind) -> Option<TaskId> {
        self.waiters_mut(kind).pop_front()
    }

    fn remove_waiter(&mut self, kind: MailboxWaitKind, task_id: TaskId) -> bool {
        let waiters = self.waiters_mut(kind);
        let Some(index) = waiters.iter().position(|queued| *queued == task_id) else {
            return false;
        };

        waiters.remove(index);
        true
    }

    fn waiters_mut(&mut self, kind: MailboxWaitKind) -> &mut VecDeque<TaskId> {
        match kind {
            MailboxWaitKind::Send => &mut self.send_waiters,
            MailboxWaitKind::Recv => &mut self.recv_waiters,
        }
    }
}

/// Commit a controlled mailbox wait from the task-call exception path.
///
/// The mailbox lock remains held through bounded waiter registration, optional
/// timeout registration, and scheduler blocking so no wakeup window is exposed.
/// Runtime queues are preallocated and this path does not allocate.
///
/// # Arguments
///
/// * `context` - Exclusive live task context for scheduler handoff.
/// * `control` - Opaque pointer to the stable mailbox control lock.
/// * `raw_wait_kind` - Private task-call tag selecting send or receive wait.
/// * `timeout_deadline` - Optional absolute counter deadline.
///
/// # Returns
///
/// Returns immediately if the mailbox no longer requires waiting or after the
/// blocked task is later resumed.
///
/// # Panics
///
/// Panics for a null control pointer, invalid wait tag, missing current task,
/// exhausted bounded waiter storage, or inconsistent scheduler state.
pub(crate) fn on_mailbox_wait_sync(
    context: &mut ActiveContext<'_>,
    control: *mut core::ffi::c_void,
    raw_wait_kind: u64,
    timeout_deadline: Option<u64>,
) {
    if control.is_null() {
        panic!("ipc: mailbox wait with null control block");
    }

    let wait_kind = MailboxWaitKind::from_raw(raw_wait_kind)
        .unwrap_or_else(|| panic!("ipc: invalid mailbox wait kind {raw_wait_kind}"));
    let task_id =
        crate::sched::current_task_id().unwrap_or_else(|| panic!("ipc: wait without task"));
    crate::sched::clear_current_wait_result();

    // SAFETY: blocking mailbox operations pass a pointer to their non-generic
    // control lock. The containing mailbox must remain alive while tasks wait.
    let control_ptr = control;
    let control = unsafe { &*(control_ptr.cast::<IrqSpinLock<MailboxControl>>()) };
    let mut control = control.lock();

    if !control.should_wait(wait_kind) {
        crate::trace!("ipc: wait skipped task {task_id} kind={wait_kind:?}");
        return;
    }

    if let Some(deadline) = timeout_deadline {
        if deadline <= crate::time::now_counter() {
            crate::trace!("ipc: wait deadline already expired task {task_id} kind={wait_kind:?}");
            crate::sched::set_current_wait_result(crate::sched::WaitResult::TimedOut);
            return;
        }
    }

    control.enqueue_waiter(wait_kind, task_id);
    if let Some(deadline) = timeout_deadline {
        crate::debug!(
            "ipc: task {task_id} blocked on mailbox {wait_kind:?} timeout_deadline={deadline}"
        );
    } else {
        crate::trace!("ipc: task {task_id} blocked on mailbox {wait_kind:?}");
    }

    // Keep the mailbox lock held through timeout registration and scheduler
    // blocking. Local IRQs stay masked, so no producer/consumer or timer IRQ can
    // observe "queued but not blocked" or "timed event without wait state".
    crate::sched::block_current_on_ipc(
        context,
        IpcWaitRegistration::mailbox(control_ptr, wait_kind, timeout_deadline),
    );
}

pub(crate) fn remove_timed_out_waiter(token: IpcWaitToken, task_id: TaskId) -> bool {
    match token.wait_object {
        IpcWaitObject::Mailbox(wait_kind) => {
            remove_timed_out_mailbox_waiter(token.object, wait_kind, task_id)
        }
    }
}

fn remove_timed_out_mailbox_waiter(
    control: *mut core::ffi::c_void,
    wait_kind: MailboxWaitKind,
    task_id: TaskId,
) -> bool {
    if control.is_null() {
        panic!("ipc: timeout cleanup with null control block");
    }

    // SAFETY: scheduler stores the same control pointer supplied by a blocking
    // mailbox wait. The mailbox owner must keep the mailbox alive and unmoved
    // until all waiters have completed or timed out.
    let control = unsafe { &*(control.cast::<IrqSpinLock<MailboxControl>>()) };
    let mut control = control.lock();
    let removed = control.remove_waiter(wait_kind, task_id);
    if removed {
        crate::debug!("ipc: removed timed-out task {task_id} from {wait_kind:?} wait queue");
    } else {
        crate::trace!("ipc: timeout cleanup found no task {task_id} in {wait_kind:?} wait queue");
    }
    removed
}
