//! Finite scheduler, timing, mailbox, and lifecycle contract scenarios.

use alloc::vec::Vec;
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use crate::{
    ipc::{Mailbox, RecvTimeoutError},
    memory,
    sched::{self, ThreadArg, ThreadAttrs},
    sync::{
        LocalIrqGuard, PreemptLock,
        preempt::{PreemptGuard, reschedule_pending},
    },
};

use super::protocol;

const SUITE: &str = "kernel-contract";
const WORKER_EXIT: usize = 0x2a;
const DELIVERY_VALUE: usize = 0x51;
const ALLOCATOR_WORKER_COUNT: usize = 3;
const ALLOCATOR_CYCLES: usize = 6;
const ALLOCATOR_HEAP_ITEMS: usize = 64;
const ALLOCATOR_WORKER_OK: usize = 0;
const ALLOCATOR_FRAME_FAILED: usize = 1;
const ALLOCATOR_RANGE_FAILED: usize = 2;
const ALLOCATOR_HEAP_FAILED: usize = 3;
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
static TIMER_IRQ_COUNT: AtomicUsize = AtomicUsize::new(0);
static GUARD_PEER_STAGE: AtomicUsize = AtomicUsize::new(0);
static GUARD_PEER_PROGRESS: AtomicUsize = AtomicUsize::new(0);
static WAKEUP_STAGE: AtomicUsize = AtomicUsize::new(0);
static WAKEUP_DEADLINE: AtomicU64 = AtomicU64::new(0);
static TEST_PREEMPT_LOCK: PreemptLock<usize> = PreemptLock::new(0);

/// Record one timer IRQ for bounded kernel-runtime contract coordination.
///
/// This test-only hook increments a lock-free counter, allocates nothing, and
/// never participates in production kernels or scheduler policy.
///
/// # Returns
///
/// Returns after recording the IRQ for the bounded contract coordinator.
pub(crate) fn note_timer_irq() {
    TIMER_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
}

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

    protocol::case_start("preempt-lock-keeps-irqs-enabled");
    run_preempt_lock_case();
    protocol::pass("preempt-lock-keeps-irqs-enabled");

    protocol::case_start("deferred-reschedule-after-unlock");
    run_deferred_reschedule_case();
    protocol::pass("deferred-reschedule-after-unlock");

    protocol::case_start("nested-preempt-guard");
    run_nested_preempt_guard_case();
    protocol::pass("nested-preempt-guard");

    protocol::case_start("irq-masked-unlock-preserves-pending");
    run_irq_masked_unlock_case();
    protocol::pass("irq-masked-unlock-preserves-pending");

    protocol::case_start("deferred-wakeup");
    run_deferred_wakeup_case();
    protocol::pass("deferred-wakeup");

    protocol::case_start("yield-inside-preempt-guard");
    run_yield_inside_preempt_guard_case();
    protocol::pass("yield-inside-preempt-guard");

    protocol::case_start("allocator-preemption-lifecycle");
    let free_frames_before = memory::free_frame_count()
        .unwrap_or_else(|| protocol::fail("allocator-preemption-lifecycle", "NOT_INITIALIZED"));
    let mut workers = [None; ALLOCATOR_WORKER_COUNT];
    for (index, worker) in workers.iter_mut().enumerate() {
        *worker = Some(
            sched::thread_spawn(
                allocator_worker,
                ThreadArg::from_usize(index + 1),
                ThreadAttrs::joinable(),
            )
            .unwrap_or_else(|_| protocol::fail("allocator-preemption-lifecycle", "SPAWN")),
        );
    }
    for worker in workers {
        if sched::thread_join(
            worker.unwrap_or_else(|| {
                protocol::fail("allocator-preemption-lifecycle", "MISSING_WORKER")
            }),
        ) != Ok(ALLOCATOR_WORKER_OK)
        {
            protocol::fail("allocator-preemption-lifecycle", "WORKER_FAILED");
        }
    }
    if memory::free_frame_count() != Some(free_frames_before) {
        protocol::fail("allocator-preemption-lifecycle", "FRAME_LEAK");
    }
    protocol::pass("allocator-preemption-lifecycle");

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

fn run_preempt_lock_case() {
    let mut lock = TEST_PREEMPT_LOCK.lock();
    let worker = prepare_guard_peer("preempt-lock-keeps-irqs-enabled");
    let baseline = TIMER_IRQ_COUNT.load(Ordering::Acquire);
    if !crate::sync::preempt::is_disabled() || !crate::sync::preempt::scheduler_online() {
        protocol::fail("preempt-lock-keeps-irqs-enabled", "BAD_PREEMPT_STATE");
    }
    *lock = lock.wrapping_add(1);
    GUARD_PEER_STAGE.store(1, Ordering::Release);
    wait_for_timer_irqs_after(baseline, 3);
    if GUARD_PEER_PROGRESS.load(Ordering::Acquire) != 0 || !reschedule_pending() {
        protocol::fail("preempt-lock-keeps-irqs-enabled", "BAD_DEFERRED_STATE");
    }
    drop(lock);
    wait_for_guard_peer("preempt-lock-keeps-irqs-enabled", worker);
}

fn run_deferred_reschedule_case() {
    let guard = PreemptGuard::enter();
    let worker = prepare_guard_peer("deferred-reschedule-after-unlock");
    GUARD_PEER_STAGE.store(1, Ordering::Release);
    crate::sync::preempt::request_reschedule();
    if GUARD_PEER_PROGRESS.load(Ordering::Acquire) != 0 || !reschedule_pending() {
        protocol::fail("deferred-reschedule-after-unlock", "BAD_DEFERRED_STATE");
    }
    drop(guard);
    // No explicit yield or timer wait occurs between outermost drop and proof.
    wait_for_guard_peer("deferred-reschedule-after-unlock", worker);
}

fn run_nested_preempt_guard_case() {
    let outer = PreemptGuard::enter();
    let inner = PreemptGuard::enter();
    let worker = prepare_guard_peer("nested-preempt-guard");
    GUARD_PEER_STAGE.store(1, Ordering::Release);
    sched::yield_now();
    drop(inner);
    if GUARD_PEER_PROGRESS.load(Ordering::Acquire) != 0 || !reschedule_pending() {
        protocol::fail("nested-preempt-guard", "INNER_DROP_CONSUMED_REQUEST");
    }
    drop(outer);
    wait_for_guard_peer("nested-preempt-guard", worker);
}

fn run_irq_masked_unlock_case() {
    let irq_guard = LocalIrqGuard::save_and_disable();
    let guard = PreemptGuard::enter();
    let worker = prepare_guard_peer("irq-masked-unlock-preserves-pending");
    GUARD_PEER_STAGE.store(1, Ordering::Release);
    crate::sync::preempt::request_reschedule();
    drop(guard);
    if GUARD_PEER_PROGRESS.load(Ordering::Acquire) != 0 || !reschedule_pending() {
        protocol::fail(
            "irq-masked-unlock-preserves-pending",
            "MASKED_DROP_SERVICED_REQUEST",
        );
    }
    drop(irq_guard);
    sched::yield_now();
    wait_for_guard_peer("irq-masked-unlock-preserves-pending", worker);
}

fn run_deferred_wakeup_case() {
    WAKEUP_STAGE.store(0, Ordering::Release);
    let worker = sched::thread_spawn(wakeup_peer, ThreadArg::empty(), ThreadAttrs::joinable())
        .unwrap_or_else(|_| protocol::fail("deferred-wakeup", "SPAWN"));
    while WAKEUP_STAGE.load(Ordering::Acquire) == 0 {
        sched::yield_now();
    }
    let deadline = WAKEUP_DEADLINE.load(Ordering::Acquire);
    if deadline == 0 {
        protocol::fail("deferred-wakeup", "NO_DEADLINE");
    }

    {
        let _guard = PreemptGuard::enter();
        while crate::time::now_counter() < deadline {
            core::hint::spin_loop();
        }
        let irq_at_deadline = TIMER_IRQ_COUNT.load(Ordering::Acquire);
        wait_for_timer_irqs_after(irq_at_deadline, 1);
        if WAKEUP_STAGE.load(Ordering::Acquire) != 1 || !reschedule_pending() {
            protocol::fail("deferred-wakeup", "WAKE_NOT_DEFERRED");
        }
    }
    if WAKEUP_STAGE.load(Ordering::Acquire) != 2 {
        protocol::fail("deferred-wakeup", "NO_UNLOCK_CHECKPOINT");
    }
    if sched::thread_join(worker) != Ok(0) {
        protocol::fail("deferred-wakeup", "JOIN");
    }
}

fn run_yield_inside_preempt_guard_case() {
    let guard = PreemptGuard::enter();
    let worker = prepare_guard_peer("yield-inside-preempt-guard");
    GUARD_PEER_STAGE.store(1, Ordering::Release);
    sched::yield_now();
    if GUARD_PEER_PROGRESS.load(Ordering::Acquire) != 0 || !reschedule_pending() {
        protocol::fail("yield-inside-preempt-guard", "YIELD_ESCAPED_GUARD");
    }
    drop(guard);
    wait_for_guard_peer("yield-inside-preempt-guard", worker);
}

fn prepare_guard_peer(case: &str) -> crate::task::ThreadId {
    GUARD_PEER_STAGE.store(0, Ordering::Release);
    GUARD_PEER_PROGRESS.store(0, Ordering::Release);
    sched::thread_spawn(guard_peer, ThreadArg::empty(), ThreadAttrs::joinable())
        .unwrap_or_else(|_| protocol::fail(case, "SPAWN"))
}

fn wait_for_guard_peer(case: &str, worker: crate::task::ThreadId) {
    if GUARD_PEER_PROGRESS.load(Ordering::Acquire) == 0
        || GUARD_PEER_STAGE.load(Ordering::Acquire) != 2
    {
        protocol::fail(case, "NO_UNLOCK_CHECKPOINT");
    }
    if reschedule_pending() {
        protocol::fail(case, "STALE_PENDING_REQUEST");
    }
    if sched::thread_join(worker) != Ok(0) {
        protocol::fail(case, "JOIN");
    }
}

fn wait_for_timer_irqs_after(baseline: usize, count: usize) {
    let target = baseline.saturating_add(count);
    while TIMER_IRQ_COUNT.load(Ordering::Acquire) < target {
        core::hint::spin_loop();
    }
}

fn guard_peer(_arg: ThreadArg) -> usize {
    while GUARD_PEER_STAGE.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    // Acquiring the same lock used by `run_preempt_lock_case` proves that the
    // previous owner publishes `locked = false` before its deferred checkpoint.
    let lock = TEST_PREEMPT_LOCK.lock();
    GUARD_PEER_PROGRESS.store(1, Ordering::Release);
    GUARD_PEER_STAGE.store(2, Ordering::Release);
    drop(lock);
    0
}

fn wakeup_peer(_arg: ThreadArg) -> usize {
    let deadline = crate::time::now_counter().wrapping_add(crate::time::ms_to_counts(2));
    WAKEUP_DEADLINE.store(deadline, Ordering::Release);
    WAKEUP_STAGE.store(1, Ordering::Release);
    sched::sleep_until_counter(deadline);
    WAKEUP_STAGE.store(2, Ordering::Release);
    0
}

fn allocator_worker(arg: ThreadArg) -> usize {
    let seed = arg.as_usize();
    for cycle in 0..ALLOCATOR_CYCLES {
        let Some(frame) = memory::alloc_frame() else {
            return ALLOCATOR_FRAME_FAILED;
        };
        let Some(range) = memory::alloc_contiguous_frames(2) else {
            memory::free_frame(frame);
            return ALLOCATOR_RANGE_FAILED;
        };

        let mut values = Vec::with_capacity(ALLOCATOR_HEAP_ITEMS);
        for item in 0..ALLOCATOR_HEAP_ITEMS {
            values.push(seed.wrapping_add(cycle).wrapping_add(item));
        }
        let heap_ok = values.len() == ALLOCATOR_HEAP_ITEMS
            && values.first() == Some(&seed.wrapping_add(cycle));
        drop(values);

        memory::free_contiguous_frames(range);
        memory::free_frame(frame);
        if !heap_ok {
            return ALLOCATOR_HEAP_FAILED;
        }

        // Yield only after releasing allocator resources so several bounded
        // worker lifecycles interleave without testing wall-clock timing.
        sched::msleep(1);
    }

    ALLOCATOR_WORKER_OK
}

fn test_mailbox() -> &'static Mailbox<usize> {
    if TEST_MAILBOX.state.load(Ordering::Acquire) != MAILBOX_READY {
        panic!("qemu-test: mailbox not initialized");
    }
    // SAFETY: acquire observes the mailbox initialized before scheduler entry.
    unsafe { (&*TEST_MAILBOX.value.get()).assume_init_ref() }
}
