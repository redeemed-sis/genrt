//! Finite scheduler, timing, mailbox, and lifecycle contract scenarios.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use crate::{
    ipc::{Mailbox, RecvTimeoutError},
    sched::{self, ThreadArg, ThreadAttrs},
};

use super::protocol;

const SUITE: &str = "kernel-contract";
const WORKER_EXIT: usize = 0x2a;
const DELIVERY_VALUE: usize = 0x51;
const MAILBOX_UNINITIALIZED: u8 = 0;
const MAILBOX_INITIALIZING: u8 = 1;
const MAILBOX_READY: u8 = 2;

struct TestMailboxCell {
    value: UnsafeCell<MaybeUninit<Mailbox<usize>>>,
    state: AtomicU8,
}

// SAFETY: init publishes the mailbox before scheduler entry. Mailbox provides
// its own IRQ-save synchronization for all runtime access.
unsafe impl Sync for TestMailboxCell {}

static TEST_MAILBOX: TestMailboxCell = TestMailboxCell {
    value: UnsafeCell::new(MaybeUninit::uninit()),
    state: AtomicU8::new(MAILBOX_UNINITIALIZED),
};
static DELIVERY_STAGE: AtomicUsize = AtomicUsize::new(0);
static DELIVERY_RESULT: AtomicUsize = AtomicUsize::new(0);
static DELIVERY_ELAPSED_MS: AtomicUsize = AtomicUsize::new(0);
static PREEMPT_STAGE: AtomicUsize = AtomicUsize::new(0);
static PROGRESS_A: AtomicUsize = AtomicUsize::new(0);
static PROGRESS_B: AtomicUsize = AtomicUsize::new(0);

/// Finite test-only scheduler tasks selected by the kernel runtime feature.
pub(crate) const TASKS: [sched::StaticTask; 4] = [
    sched::StaticTask::new(coordinator, ThreadArg::empty()),
    sched::StaticTask::new(blocked_receiver, ThreadArg::empty()),
    sched::StaticTask::new(progress_a, ThreadArg::empty()),
    sched::StaticTask::new(progress_b, ThreadArg::empty()),
];

/// Initialize bounded fixtures before scheduler entry.
///
/// # Returns
///
/// Returns after publishing the mailbox used by the finite test tasks.
///
/// # Panics
///
/// Panics if called more than once.
pub(crate) fn init() {
    if TEST_MAILBOX
        .state
        .compare_exchange(
            MAILBOX_UNINITIALIZED,
            MAILBOX_INITIALIZING,
            Ordering::Acquire,
            Ordering::Relaxed,
        )
        .is_err()
    {
        panic!("qemu-test: runtime fixtures initialized twice");
    }
    // SAFETY: this runs once before any static task can access the cell.
    unsafe {
        (*TEST_MAILBOX.value.get()).write(Mailbox::with_capacity(1, TASKS.len() + 1));
    }
    TEST_MAILBOX.state.store(MAILBOX_READY, Ordering::Release);
}

fn coordinator(_arg: ThreadArg) -> usize {
    protocol::ready(SUITE);

    protocol::case_start("sleep-deadline");
    let started = crate::time::uptime_ms();
    sched::msleep(25);
    if crate::time::uptime_ms().saturating_sub(started) < 25 {
        protocol::fail("sleep-deadline", "EARLY_WAKE");
    }
    protocol::pass("sleep-deadline");

    protocol::case_start("mailbox-blocked-wakeup");
    DELIVERY_STAGE.store(1, Ordering::Release);
    while DELIVERY_STAGE.load(Ordering::Acquire) != 2 {
        sched::msleep(1);
    }
    sched::msleep(25);
    test_mailbox().send(DELIVERY_VALUE);
    while DELIVERY_STAGE.load(Ordering::Acquire) != 3 {
        sched::msleep(1);
    }
    if DELIVERY_RESULT.load(Ordering::Acquire) != DELIVERY_VALUE
        || DELIVERY_ELAPSED_MS.load(Ordering::Acquire) < 20
    {
        protocol::fail("mailbox-blocked-wakeup", "NOT_BLOCKED");
    }
    protocol::pass("mailbox-blocked-wakeup");

    protocol::case_start("mailbox-timeout");
    let started = crate::time::uptime_ms();
    if test_mailbox().recv_timeout_ms(25) != Err(RecvTimeoutError::Timeout)
        || crate::time::uptime_ms().saturating_sub(started) < 25
    {
        protocol::fail("mailbox-timeout", "EARLY_OR_DELIVERED");
    }
    protocol::pass("mailbox-timeout");

    protocol::case_start("thread-join");
    let worker = sched::thread_spawn(worker, ThreadArg::empty(), ThreadAttrs::joinable())
        .unwrap_or_else(|_| protocol::fail("thread-join", "SPAWN"));
    if sched::thread_join(worker) != Ok(WORKER_EXIT) {
        protocol::fail("thread-join", "BAD_STATUS");
    }
    protocol::pass("thread-join");

    protocol::case_start("timer-preemption");
    PREEMPT_STAGE.store(1, Ordering::Release);
    sched::msleep(50);
    PREEMPT_STAGE.store(2, Ordering::Release);
    if PROGRESS_A.load(Ordering::Acquire) == 0 || PROGRESS_B.load(Ordering::Acquire) == 0 {
        protocol::fail("timer-preemption", "NO_PEER_PROGRESS");
    }
    protocol::pass("timer-preemption");

    protocol::case_start("post-test-liveness");
    sched::msleep(10);
    protocol::pass("post-test-liveness");
    protocol::done(SUITE)
}

fn blocked_receiver(_arg: ThreadArg) -> usize {
    while DELIVERY_STAGE.load(Ordering::Acquire) != 1 {
        sched::msleep(1);
    }
    let started = crate::time::uptime_ms();
    DELIVERY_STAGE.store(2, Ordering::Release);
    let value = test_mailbox().recv_timeout_ms(200).unwrap_or(0);
    DELIVERY_ELAPSED_MS.store(
        crate::time::uptime_ms().saturating_sub(started) as usize,
        Ordering::Release,
    );
    DELIVERY_RESULT.store(value, Ordering::Release);
    DELIVERY_STAGE.store(3, Ordering::Release);
    0
}

fn progress_a(_arg: ThreadArg) -> usize {
    spin_progress(&PROGRESS_A)
}

fn progress_b(_arg: ThreadArg) -> usize {
    spin_progress(&PROGRESS_B)
}

fn spin_progress(counter: &AtomicUsize) -> usize {
    while PREEMPT_STAGE.load(Ordering::Acquire) == 0 {
        sched::msleep(1);
    }
    while PREEMPT_STAGE.load(Ordering::Acquire) == 1 {
        counter.fetch_add(1, Ordering::Relaxed);
        core::hint::spin_loop();
    }
    0
}

fn worker(_arg: ThreadArg) -> usize {
    sched::msleep(10);
    WORKER_EXIT
}

fn test_mailbox() -> &'static Mailbox<usize> {
    if TEST_MAILBOX.state.load(Ordering::Acquire) != MAILBOX_READY {
        panic!("qemu-test: mailbox not initialized");
    }
    // SAFETY: acquire observes the mailbox initialized before scheduler entry.
    unsafe { (&*TEST_MAILBOX.value.get()).assume_init_ref() }
}
