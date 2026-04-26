use alloc::{collections::VecDeque, vec::Vec};
use core::cell::UnsafeCell;

use crate::{sync::IrqSpinLock, task::TaskId};

const MAILBOX_WAIT_SEND: u64 = 1;
const MAILBOX_WAIT_RECV: u64 = 2;

#[derive(Debug, Eq, PartialEq)]
pub enum SendError<T> {
    Full(T),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecvError {
    Empty,
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

/// Bounded mailbox for kernel-task IPC.
///
/// Blocking `send`/`recv` pass a pointer to this mailbox's control block through
/// the synchronous task-call path. The mailbox object must therefore remain
/// alive and must not be moved while any task may be waiting on it.
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
                crate::sched::wake_task(task_id);
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
                crate::sched::wake_task(task_id);
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

    fn wait(&self, wait_kind: MailboxWaitKind) {
        crate::task_call::mailbox_wait(
            &self.control as *const IrqSpinLock<MailboxControl> as *const core::ffi::c_void,
            wait_kind.raw(),
        );
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

    fn waiters_mut(&mut self, kind: MailboxWaitKind) -> &mut VecDeque<TaskId> {
        match kind {
            MailboxWaitKind::Send => &mut self.send_waiters,
            MailboxWaitKind::Recv => &mut self.recv_waiters,
        }
    }
}

pub fn on_mailbox_wait_sync(
    active_frame_words: *mut u64,
    control: *mut core::ffi::c_void,
    raw_wait_kind: u64,
) {
    if active_frame_words.is_null() {
        panic!("ipc: mailbox wait without active frame");
    }
    if control.is_null() {
        panic!("ipc: mailbox wait with null control block");
    }

    let wait_kind = MailboxWaitKind::from_raw(raw_wait_kind)
        .unwrap_or_else(|| panic!("ipc: invalid mailbox wait kind {raw_wait_kind}"));
    let task_id =
        crate::sched::current_task_id().unwrap_or_else(|| panic!("ipc: wait without task"));

    // SAFETY: blocking mailbox operations pass a pointer to their non-generic
    // control lock. The containing mailbox must remain alive while tasks wait.
    let control = unsafe { &*(control.cast::<IrqSpinLock<MailboxControl>>()) };
    let mut control = control.lock();

    if !control.should_wait(wait_kind) {
        crate::trace!("ipc: wait skipped task {task_id} kind={wait_kind:?}");
        return;
    }

    control.enqueue_waiter(wait_kind, task_id);
    crate::trace!("ipc: task {task_id} blocked on mailbox {wait_kind:?}");
    crate::sched::block_current_on_ipc(active_frame_words);
}
