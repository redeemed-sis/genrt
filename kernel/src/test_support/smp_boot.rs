//! Test-only coordinator for four-CPU scheduling and scheduler IPIs.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    cpu,
    ipc::Mailbox,
    sched,
    sync::{IrqSpinLock, LocalIrqGuard},
    time,
};

use super::{
    protocol,
    scenario::{self, ScenarioResult},
};

const SUITE: &str = "smp-boot";
const WORKER_TIMEOUT_MS: u64 = 2_000;
const COALESCED_WORKERS: usize = 3;
const MAILBOX_VALUE: usize = 0x4754_5254;

static PARALLEL_BARRIER: AtomicUsize = AtomicUsize::new(0);
static PARALLEL_FINISHED: AtomicUsize = AtomicUsize::new(0);
static CPU1_A_PROGRESS: AtomicUsize = AtomicUsize::new(0);
static CPU1_B_PROGRESS: AtomicUsize = AtomicUsize::new(0);
static REMOTE_BUSY_READY: AtomicUsize = AtomicUsize::new(0);
static REMOTE_BUSY_PEER_PROGRESS: AtomicUsize = AtomicUsize::new(0);
static REMOTE_IDLE_PROGRESS: AtomicUsize = AtomicUsize::new(0);
static COALESCING_GATE_READY: AtomicUsize = AtomicUsize::new(0);
static COALESCING_GATE_RELEASE: AtomicUsize = AtomicUsize::new(0);
static COALESCED_COMPLETED: [AtomicUsize; COALESCED_WORKERS] =
    [const { AtomicUsize::new(0) }; COALESCED_WORKERS];
static MAILBOX_RECEIVED: AtomicUsize = AtomicUsize::new(0);
static CPU1_RR_WORKERS: IrqSpinLock<Option<(sched::ThreadId, sched::ThreadId)>> =
    IrqSpinLock::new(None);

scenario::kernel_test_suite! {
    suite: SUITE,
    threads: THREADS,
    scenarios: [
        "per-cpu-scheduler-state" => per_cpu_scheduler_state,
        "parallel-pinned-workers" => parallel_pinned_workers,
        "cpu1-timer-round-robin" => cpu1_timer_round_robin,
        "remote-ipi-busy" => remote_ipi_busy,
        "remote-ipi-idle" => remote_ipi_idle,
        "remote-ipi-coalescing" => remote_ipi_coalescing,
        "cross-cpu-mailbox-wake" => cross_cpu_mailbox_wake,
        "per-cpu-timer-deadlines" => per_cpu_timer_deadlines,
    ],
}

/// Verifies the four-CPU boot, architecture, and scheduler ownership baseline.
///
/// The scenario checks topology registration, CPU0-only device routing, one
/// online scheduler context per registered CPU, and the coordinator's
/// immutable CPU0 placement before any workload tests begin.
///
/// # Returns
///
/// Returns `Ok(())` after all baseline invariants hold, or `Err("CURRENT_CPU")`
/// if the coordinator is not running on CPU0. Lower-level validation failures
/// abort through their invariant diagnostics.
fn per_cpu_scheduler_state() -> ScenarioResult {
    cpu::validate_smp_boot_for_test();
    cpu::validate_smp_device_routing_for_test();
    sched::validate_registered_contexts_online_for_test(cpu::registered_count());
    verify_current_cpu_home(cpu_id(0))?;
    Ok(())
}

/// Verifies simultaneous execution of explicitly pinned secondary workers.
///
/// CPU1 and CPU2 workers rendezvous through atomics without yielding. Both
/// must reach and leave the barrier on their immutable home CPU before the
/// coordinator can join them.
///
/// # Returns
///
/// Returns `Ok(())` after both workers rendezvous and are joined. Returns a
/// stable spawn/join/current-CPU reason or `Err("BARRIER_INCOMPLETE")` when the
/// atomic rendezvous did not complete exactly twice.
fn parallel_pinned_workers() -> ScenarioResult {
    let cpu1 = cpu_id(1);
    let cpu2 = cpu_id(2);
    let first = spawn_on(cpu1, parallel_worker)?;
    let second = spawn_on(cpu2, parallel_worker)?;
    join_worker(first, "PARALLEL_CPU1_JOIN")?;
    join_worker(second, "PARALLEL_CPU2_JOIN")?;
    if PARALLEL_BARRIER.load(Ordering::Acquire) != 2
        || PARALLEL_FINISHED.load(Ordering::Acquire) != 2
    {
        return Err("BARRIER_INCOMPLETE");
    }
    Ok(())
}

/// Verifies timer-driven round-robin between two CPU1-bound busy workers.
///
/// A CPU1 launcher creates two non-yielding peers locally. Both must make
/// progress, proving that CPU1's physical timer and local RR handoff operate
/// independently of CPU0.
///
/// # Returns
///
/// Returns `Ok(())` after both CPU1 workers make progress and all three test
/// threads are joined. Returns a stable spawn/join/timeout reason or
/// `Err("NO_RR_PROGRESS")` when either worker never ran.
fn cpu1_timer_round_robin() -> ScenarioResult {
    let cpu1 = cpu_id(1);
    let launcher = spawn_on(cpu1, cpu1_rr_launcher)?;
    let (first, second) = wait_for_cpu1_rr_workers()?;
    join_worker(launcher, "CPU1_LAUNCHER_JOIN")?;
    join_worker(first, "CPU1_A_JOIN")?;
    join_worker(second, "CPU1_B_JOIN")?;
    if CPU1_A_PROGRESS.load(Ordering::Acquire) == 0 || CPU1_B_PROGRESS.load(Ordering::Acquire) == 0
    {
        return Err("NO_RR_PROGRESS");
    }
    Ok(())
}

/// Verifies IPI-driven preemption of a busy remote CPU without polling.
///
/// CPU3 first runs one non-yielding thread with no RR deadline. CPU0 then
/// publishes a peer to CPU3; only the targeted scheduler SGI can create the
/// checkpoint that lets the peer run and release the original busy thread.
///
/// # Returns
///
/// Returns `Ok(())` after a targeted SGI preempts the busy CPU3 thread and its
/// peer executes exactly once. Returns a stable spawn/join/timeout reason or
/// `Err("NO_BUSY_TARGET_PREEMPTION")` when no target IPI progress is observed.
fn remote_ipi_busy() -> ScenarioResult {
    let cpu3 = cpu_id(3);
    let busy = spawn_on(cpu3, remote_busy_worker)?;
    wait_for_flag(&REMOTE_BUSY_READY, "BUSY_START_TIMEOUT")?;
    let before = stable_scheduler_ipi_received_counts()?;
    let peer = spawn_on(cpu3, remote_busy_peer)?;
    join_worker(busy, "BUSY_JOIN")?;
    join_worker(peer, "BUSY_PEER_JOIN")?;
    let after = scheduler_ipi_received_counts();
    if REMOTE_BUSY_PEER_PROGRESS.load(Ordering::Acquire) != 1 || after[3] <= before[3] {
        return Err("NO_BUSY_TARGET_PREEMPTION");
    }
    Ok(())
}

/// Verifies immediate targeted wakeup of an idle secondary CPU.
///
/// After CPU3 becomes quiescent, CPU0 publishes one worker. The worker
/// confirms that no polling quantum exists, while receive counters prove
/// delivery to CPU3 without perturbing CPU1 or CPU2.
///
/// # Returns
///
/// Returns `Ok(())` after CPU3 leaves idle and runs the remote worker. Returns
/// a stable spawn/join/timeout reason, `Err("NO_TARGET_IPI_PROGRESS")`, or
/// `Err("IPI_ROUTING_ISOLATION")` when another secondary receives the wakeup.
fn remote_ipi_idle() -> ScenarioResult {
    let cpu3 = cpu_id(3);
    let before = stable_scheduler_ipi_received_counts()?;
    let remote = spawn_on(cpu3, remote_idle_worker)?;
    join_worker(remote, "REMOTE_IDLE_JOIN")?;
    let after = scheduler_ipi_received_counts();
    if REMOTE_IDLE_PROGRESS.load(Ordering::Acquire) != 1 || after[3] <= before[3] {
        return Err("NO_TARGET_IPI_PROGRESS");
    }
    if after[1] != before[1] || after[2] != before[2] {
        return Err("IPI_ROUTING_ISOLATION");
    }
    Ok(())
}

/// Verifies one notification edge can cover several remote publications.
///
/// A CPU2 gate masks local IRQs while CPU0 publishes three workers. The
/// scheduler must issue one targeted SGI, retain every thread in bounded
/// ingress, and execute each worker exactly once after the gate releases.
///
/// # Returns
///
/// Returns `Ok(())` after one send edge covers all three publications and each
/// worker executes once. Returns a stable spawn/join/timeout reason,
/// `Err("SEND_EDGE_NOT_COALESCED")`, or `Err("DUPLICATE_OR_MISSING_RUN")`.
fn remote_ipi_coalescing() -> ScenarioResult {
    let cpu2 = cpu_id(2);
    let gate = spawn_on(cpu2, coalescing_gate)?;
    wait_for_flag(&COALESCING_GATE_READY, "GATE_START_TIMEOUT")?;
    let sends_before = cpu::scheduler_ipi_sent_count_for_test(cpu2);
    let mut workers = [None; COALESCED_WORKERS];
    for (index, worker) in workers.iter_mut().enumerate() {
        *worker = Some(spawn_on_arg(
            cpu2,
            coalesced_worker,
            sched::ThreadArg::from_usize(index),
        )?);
    }
    let sends_after = cpu::scheduler_ipi_sent_count_for_test(cpu2);
    if sends_after != sends_before + 1 {
        return Err("SEND_EDGE_NOT_COALESCED");
    }
    COALESCING_GATE_RELEASE.store(1, Ordering::Release);
    join_worker(gate, "GATE_JOIN")?;
    for worker in workers.into_iter().flatten() {
        join_worker(worker, "COALESCED_JOIN")?;
    }
    if COALESCED_COMPLETED
        .iter()
        .any(|completed| completed.load(Ordering::Acquire) != 1)
    {
        return Err("DUPLICATE_OR_MISSING_RUN");
    }
    Ok(())
}

/// Verifies cross-CPU wakeup through the existing mailbox wait lifecycle.
///
/// A CPU1 receiver first publishes its exact wait token and blocks. CPU0
/// sends one value, which must complete the wait through centralized remote
/// wakeup, preserve the payload, and permit a cross-CPU join.
///
/// # Returns
///
/// Returns `Ok(())` after the blocked CPU1 receiver obtains the exact value and
/// is joined from CPU0. Returns a stable spawn/join/timeout reason or
/// `Err("MAILBOX_VALUE")` when payload ownership is corrupted.
fn cross_cpu_mailbox_wake() -> ScenarioResult {
    let cpu1 = cpu_id(1);
    let mailbox = Mailbox::with_capacity(1, 1);
    let receiver = spawn_on_arg(
        cpu1,
        mailbox_receiver,
        sched::ThreadArg::from_usize(&mailbox as *const Mailbox<usize> as usize),
    )?;
    wait_for_mailbox_waiter(&mailbox)?;
    mailbox.send(MAILBOX_VALUE);
    join_worker(receiver, "MAILBOX_JOIN")?;
    if MAILBOX_RECEIVED.load(Ordering::Acquire) != MAILBOX_VALUE {
        return Err("MAILBOX_VALUE");
    }
    Ok(())
}

/// Verifies explicit one-shot timer deadlines on every logical CPU.
///
/// CPU0 and one worker on each secondary sleep twice. All wakeups must
/// preserve home-CPU ownership and produce the expected local timer probes
/// without reintroducing scheduler polling deadlines.
///
/// # Returns
///
/// Returns `Ok(())` after every CPU completes two explicit sleep deadlines and
/// reports local timer activity. Returns a stable spawn/join/current-CPU reason
/// or `Err("MISSING_TIMER_IRQ")` when a per-CPU timer probe is incomplete.
fn per_cpu_timer_deadlines() -> ScenarioResult {
    let cpu1 = cpu_id(1);
    let cpu2 = cpu_id(2);
    let cpu3 = cpu_id(3);
    let timer_workers = [
        spawn_on(cpu1, timer_worker)?,
        spawn_on(cpu2, timer_worker)?,
        spawn_on(cpu3, timer_worker)?,
    ];
    for _ in 0..2 {
        sched::msleep(2);
        verify_current_cpu_home(cpu_id(0))?;
    }
    for worker in timer_workers {
        join_worker(worker, "TIMER_JOIN")?;
    }
    if !time::local_timer_probes_complete_for_test(cpu::registered_count()) {
        return Err("MISSING_TIMER_IRQ");
    }
    sched::validate_registered_contexts_online_for_test(cpu::registered_count());
    Ok(())
}

fn parallel_worker(arg: sched::ThreadArg) -> usize {
    let expected_cpu = cpu_id(arg.as_usize());
    if expected_cpu.index() != 1 && expected_cpu.index() != 2 {
        protocol::fail("parallel-pinned-workers", "WRONG_CPU");
    }
    assert_current_cpu_home(expected_cpu, "parallel-pinned-workers");
    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    PARALLEL_BARRIER.fetch_add(1, Ordering::AcqRel);
    while PARALLEL_BARRIER.load(Ordering::Acquire) != 2 {
        assert_current_cpu_home(expected_cpu, "parallel-pinned-workers");
        if time::uptime_ms() >= deadline {
            protocol::fail("parallel-pinned-workers", "BARRIER_TIMEOUT");
        }
        core::hint::spin_loop();
    }
    assert_current_cpu_home(expected_cpu, "parallel-pinned-workers");
    PARALLEL_FINISHED.fetch_add(1, Ordering::AcqRel);
    0
}

fn cpu1_non_yielding_a(_arg: sched::ThreadArg) -> usize {
    cpu1_non_yielding_worker(&CPU1_A_PROGRESS, &CPU1_B_PROGRESS)
}

fn cpu1_rr_launcher(arg: sched::ThreadArg) -> usize {
    let expected_cpu = cpu_id(arg.as_usize());
    assert_current_cpu_home(expected_cpu, "cpu1-timer-round-robin");
    let first = sched::thread_spawn(
        cpu1_non_yielding_a,
        sched::ThreadArg::empty(),
        sched::ThreadAttrs::joinable(),
    )
    .unwrap_or_else(|_| protocol::fail("cpu1-timer-round-robin", "CPU1_A_SPAWN"));
    let second = sched::thread_spawn(
        cpu1_non_yielding_b,
        sched::ThreadArg::empty(),
        sched::ThreadAttrs::joinable(),
    )
    .unwrap_or_else(|_| protocol::fail("cpu1-timer-round-robin", "CPU1_B_SPAWN"));
    *CPU1_RR_WORKERS.lock() = Some((first, second));
    0
}

fn cpu1_non_yielding_b(_arg: sched::ThreadArg) -> usize {
    cpu1_non_yielding_worker(&CPU1_B_PROGRESS, &CPU1_A_PROGRESS)
}

fn cpu1_non_yielding_worker(progress: &AtomicUsize, peer: &AtomicUsize) -> usize {
    let cpu = cpu_id(1);
    assert_current_cpu_home(cpu, "cpu1-timer-round-robin");
    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    progress.fetch_add(1, Ordering::AcqRel);
    while peer.load(Ordering::Acquire) == 0 {
        assert_current_cpu_home(cpu, "cpu1-timer-round-robin");
        if time::uptime_ms() >= deadline {
            protocol::fail("cpu1-timer-round-robin", "RR_TIMEOUT");
        }
        core::hint::spin_loop();
    }
    assert_current_cpu_home(cpu, "cpu1-timer-round-robin");
    0
}

fn remote_busy_worker(_arg: sched::ThreadArg) -> usize {
    let cpu = cpu_id(3);
    assert_current_cpu_home(cpu, "remote-ipi-busy");
    sched::validate_no_polling_quantum_for_test();
    REMOTE_BUSY_READY.store(1, Ordering::Release);

    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    while REMOTE_BUSY_PEER_PROGRESS.load(Ordering::Acquire) == 0 {
        assert_current_cpu_home(cpu, "remote-ipi-busy");
        if time::uptime_ms() >= deadline {
            protocol::fail("remote-ipi-busy", "PEER_PREEMPT_TIMEOUT");
        }
        core::hint::spin_loop();
    }
    0
}

fn remote_busy_peer(_arg: sched::ThreadArg) -> usize {
    let cpu = cpu_id(3);
    assert_current_cpu_home(cpu, "remote-ipi-busy");
    REMOTE_BUSY_PEER_PROGRESS.fetch_add(1, Ordering::AcqRel);
    0
}

fn remote_idle_worker(_arg: sched::ThreadArg) -> usize {
    let cpu = cpu_id(3);
    assert_current_cpu_home(cpu, "remote-ipi-idle");
    sched::validate_no_polling_quantum_for_test();
    REMOTE_IDLE_PROGRESS.fetch_add(1, Ordering::AcqRel);
    0
}

fn coalescing_gate(_arg: sched::ThreadArg) -> usize {
    let cpu = cpu_id(2);
    assert_current_cpu_home(cpu, "remote-ipi-coalescing");
    let irq_guard = LocalIrqGuard::save_and_disable();
    sched::validate_no_polling_quantum_for_test();
    COALESCING_GATE_READY.store(1, Ordering::Release);

    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    while COALESCING_GATE_RELEASE.load(Ordering::Acquire) == 0 {
        assert_current_cpu_home(cpu, "remote-ipi-coalescing");
        if time::uptime_ms() >= deadline {
            protocol::fail("remote-ipi-coalescing", "GATE_RELEASE_TIMEOUT");
        }
        core::hint::spin_loop();
    }
    drop(irq_guard);
    0
}

fn coalesced_worker(arg: sched::ThreadArg) -> usize {
    let index = arg.as_usize();
    let Some(completed) = COALESCED_COMPLETED.get(index) else {
        protocol::fail("remote-ipi-coalescing", "WORKER_INDEX");
    };
    let cpu = cpu_id(2);
    assert_current_cpu_home(cpu, "remote-ipi-coalescing");
    if completed.fetch_add(1, Ordering::AcqRel) != 0 {
        protocol::fail("remote-ipi-coalescing", "DUPLICATE_RUN");
    }
    0
}

fn mailbox_receiver(arg: sched::ThreadArg) -> usize {
    let cpu = cpu_id(1);
    assert_current_cpu_home(cpu, "cross-cpu-mailbox-wake");
    // SAFETY: the CPU0 coordinator keeps its stack-owned mailbox alive until
    // this joinable worker exits and is joined.
    let mailbox = unsafe { &*(arg.as_usize() as *const Mailbox<usize>) };
    let value = mailbox.recv();
    MAILBOX_RECEIVED.store(value, Ordering::Release);
    0
}

fn timer_worker(arg: sched::ThreadArg) -> usize {
    let expected_cpu = cpu_id(arg.as_usize());
    assert_current_cpu_home(expected_cpu, "per-cpu-timer-deadlines");
    for _ in 0..2 {
        sched::msleep(2);
        assert_current_cpu_home(expected_cpu, "per-cpu-timer-deadlines");
    }
    0
}

fn spawn_on(cpu: cpu::CpuId, entry: sched::ThreadEntry) -> ScenarioResult<sched::ThreadId> {
    spawn_on_arg(cpu, entry, sched::ThreadArg::from_usize(cpu.index()))
}

fn spawn_on_arg(
    cpu: cpu::CpuId,
    entry: sched::ThreadEntry,
    arg: sched::ThreadArg,
) -> ScenarioResult<sched::ThreadId> {
    sched::thread_spawn(
        entry,
        arg,
        sched::ThreadAttrs::joinable().with_affinity(cpu),
    )
    .map_err(|_| "SPAWN")
}

fn join_worker(worker: sched::ThreadId, reason: &'static str) -> ScenarioResult {
    sched::thread_join(worker).map_err(|_| reason)?;
    verify_current_cpu_home(cpu_id(0))
}

fn wait_for_cpu1_rr_workers() -> ScenarioResult<(sched::ThreadId, sched::ThreadId)> {
    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    loop {
        if let Some(workers) = *CPU1_RR_WORKERS.lock() {
            return Ok(workers);
        }
        if time::uptime_ms() >= deadline {
            return Err("LAUNCH_TIMEOUT");
        }
        verify_current_cpu_home(cpu_id(0))?;
        core::hint::spin_loop();
    }
}

fn wait_for_mailbox_waiter(mailbox: &Mailbox<usize>) -> ScenarioResult {
    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    while mailbox.waiter_count_for_test() == 0 {
        if time::uptime_ms() >= deadline {
            return Err("WAITER_TIMEOUT");
        }
        verify_current_cpu_home(cpu_id(0))?;
        core::hint::spin_loop();
    }
    Ok(())
}

fn wait_for_flag(flag: &AtomicUsize, timeout_reason: &'static str) -> ScenarioResult {
    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    while flag.load(Ordering::Acquire) == 0 {
        if time::uptime_ms() >= deadline {
            return Err(timeout_reason);
        }
        verify_current_cpu_home(cpu_id(0))?;
        core::hint::spin_loop();
    }
    Ok(())
}

fn scheduler_ipi_received_counts() -> [usize; 4] {
    core::array::from_fn(|index| cpu::scheduler_ipi_received_count_for_test(cpu_id(index)))
}

fn stable_scheduler_ipi_received_counts() -> ScenarioResult<[usize; 4]> {
    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    let mut previous = scheduler_ipi_received_counts();
    loop {
        sched::msleep(1);
        let current = scheduler_ipi_received_counts();
        if current == previous {
            return Ok(current);
        }
        if time::uptime_ms() >= deadline {
            return Err("IPI_QUIESCE_TIMEOUT");
        }
        previous = current;
    }
}

fn verify_current_cpu_home(expected: cpu::CpuId) -> ScenarioResult {
    if cpu::current_id() != Ok(expected) {
        return Err("CURRENT_CPU");
    }
    sched::validate_current_home_cpu_for_test();
    Ok(())
}

fn assert_current_cpu_home(expected: cpu::CpuId, case: &str) {
    if cpu::current_id() != Ok(expected) {
        protocol::fail(case, "CURRENT_CPU");
    }
    sched::validate_current_home_cpu_for_test();
}

fn cpu_id(index: usize) -> cpu::CpuId {
    cpu::CpuId::from_index(index)
        .unwrap_or_else(|| protocol::fail("per-cpu-scheduler-state", "CPU_INDEX"))
}
