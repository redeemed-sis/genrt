//! Test-only coordinator for four-CPU scheduler activation.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{cpu, sched, sync::IrqSpinLock, time};

use super::protocol;

const SUITE: &str = "smp-boot";
const WORKER_TIMEOUT_MS: u64 = 2_000;

static PARALLEL_BARRIER: AtomicUsize = AtomicUsize::new(0);
static PARALLEL_FINISHED: AtomicUsize = AtomicUsize::new(0);
static CPU1_A_PROGRESS: AtomicUsize = AtomicUsize::new(0);
static CPU1_B_PROGRESS: AtomicUsize = AtomicUsize::new(0);
static PARALLEL_WORKER_EXECUTION: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];
static CPU1_LAUNCHER_EXECUTION: AtomicUsize = AtomicUsize::new(0);
static CPU1_A_EXECUTION: AtomicUsize = AtomicUsize::new(0);
static CPU1_B_EXECUTION: AtomicUsize = AtomicUsize::new(0);
static CPU3_A_RUNNING: AtomicUsize = AtomicUsize::new(0);
static CPU3_B_PROGRESS: AtomicUsize = AtomicUsize::new(0);
static CPU3_A_EXECUTION: AtomicUsize = AtomicUsize::new(0);
static CPU3_B_EXECUTION: AtomicUsize = AtomicUsize::new(0);
static CPU1_RR_WORKERS: IrqSpinLock<Option<(sched::ThreadId, sched::ThreadId)>> =
    IrqSpinLock::new(None);

struct WorkerExecutionGuard {
    executing: &'static AtomicUsize,
}

impl WorkerExecutionGuard {
    fn acquire(
        executing: &'static AtomicUsize,
        expected_cpu: cpu::CpuId,
        case: &'static str,
    ) -> Self {
        assert_current_cpu_home(expected_cpu, case);
        if executing
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            protocol::fail(case, "REENTRANCY");
        }
        Self { executing }
    }
}

impl Drop for WorkerExecutionGuard {
    fn drop(&mut self) {
        if self.executing.swap(0, Ordering::AcqRel) != 1 {
            protocol::fail("per-cpu-scheduler-state", "GUARD_CORRUPT");
        }
    }
}

/// Single CPU0 coordinator selected by the SMP boot QEMU feature.
pub(crate) const THREADS: [sched::StaticThread; 1] = [sched::StaticThread::new(
    coordinator,
    sched::ThreadArg::empty(),
)];

fn coordinator(_arg: sched::ThreadArg) -> usize {
    protocol::ready(SUITE);

    protocol::case_start("per-cpu-scheduler-state");
    cpu::validate_smp_boot_for_test();
    cpu::validate_smp_device_routing_for_test();
    sched::validate_registered_contexts_online_for_test(cpu::registered_count());
    assert_current_cpu_home(cpu_id(0), "per-cpu-scheduler-state");
    protocol::pass("per-cpu-scheduler-state");

    protocol::case_start("parallel-pinned-workers");
    let cpu1 = cpu_id(1);
    let cpu2 = cpu_id(2);
    let first = spawn_on(cpu1, parallel_worker, "parallel-pinned-workers");
    let second = spawn_on(cpu2, parallel_worker, "parallel-pinned-workers");
    join_worker(first, "parallel-pinned-workers", "PARALLEL_CPU1_JOIN");
    join_worker(second, "parallel-pinned-workers", "PARALLEL_CPU2_JOIN");
    if PARALLEL_BARRIER.load(Ordering::Acquire) != 2
        || PARALLEL_FINISHED.load(Ordering::Acquire) != 2
    {
        protocol::fail("parallel-pinned-workers", "BARRIER_INCOMPLETE");
    }
    protocol::pass("parallel-pinned-workers");

    protocol::case_start("cpu1-timer-round-robin");
    let launcher = spawn_on(cpu1, cpu1_rr_launcher, "cpu1-timer-round-robin");
    let (first, second) = wait_for_cpu1_rr_workers();
    join_worker(launcher, "cpu1-timer-round-robin", "CPU1_LAUNCHER_JOIN");
    join_worker(first, "cpu1-timer-round-robin", "CPU1_A_JOIN");
    join_worker(second, "cpu1-timer-round-robin", "CPU1_B_JOIN");
    if CPU1_A_PROGRESS.load(Ordering::Acquire) == 0 || CPU1_B_PROGRESS.load(Ordering::Acquire) == 0
    {
        protocol::fail("cpu1-timer-round-robin", "NO_RR_PROGRESS");
    }
    protocol::pass("cpu1-timer-round-robin");

    protocol::case_start("late-remote-ready");
    let cpu3 = cpu_id(3);
    let first = spawn_on(cpu3, cpu3_non_yielding_a, "late-remote-ready");
    wait_for_progress(&CPU3_A_RUNNING, "late-remote-ready", "CPU3_A_START");
    let second = spawn_on(cpu3, cpu3_remote_b, "late-remote-ready");
    join_worker(first, "late-remote-ready", "CPU3_A_JOIN");
    join_worker(second, "late-remote-ready", "CPU3_B_JOIN");
    if CPU3_B_PROGRESS.load(Ordering::Acquire) == 0 {
        protocol::fail("late-remote-ready", "NO_REMOTE_PROGRESS");
    }
    protocol::pass("late-remote-ready");

    protocol::case_start("per-cpu-timer-irqs");
    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    while !time::local_timer_probes_complete_for_test(cpu::registered_count()) {
        if time::uptime_ms() >= deadline {
            protocol::fail("per-cpu-timer-irqs", "TIMEOUT");
        }
        assert_current_cpu_home(cpu_id(0), "per-cpu-timer-irqs");
        core::hint::spin_loop();
    }
    sched::validate_registered_contexts_online_for_test(cpu::registered_count());
    protocol::pass("per-cpu-timer-irqs");
    protocol::done(SUITE)
}

fn parallel_worker(arg: sched::ThreadArg) -> usize {
    let expected_cpu = cpu_id(arg.as_usize());
    if expected_cpu.index() != 1 && expected_cpu.index() != 2 {
        protocol::fail("parallel-pinned-workers", "WRONG_CPU");
    }
    let _execution = WorkerExecutionGuard::acquire(
        &PARALLEL_WORKER_EXECUTION[expected_cpu.index()],
        expected_cpu,
        "parallel-pinned-workers",
    );
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
    cpu1_non_yielding_worker(&CPU1_A_EXECUTION, &CPU1_A_PROGRESS, &CPU1_B_PROGRESS)
}

fn cpu1_rr_launcher(arg: sched::ThreadArg) -> usize {
    let expected_cpu = cpu_id(arg.as_usize());
    let _execution = WorkerExecutionGuard::acquire(
        &CPU1_LAUNCHER_EXECUTION,
        expected_cpu,
        "cpu1-timer-round-robin",
    );
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
    cpu1_non_yielding_worker(&CPU1_B_EXECUTION, &CPU1_B_PROGRESS, &CPU1_A_PROGRESS)
}

fn cpu1_non_yielding_worker(
    execution: &'static AtomicUsize,
    progress: &AtomicUsize,
    peer: &AtomicUsize,
) -> usize {
    let cpu = cpu_id(1);
    let _execution = WorkerExecutionGuard::acquire(execution, cpu, "cpu1-timer-round-robin");
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

fn cpu3_non_yielding_a(_arg: sched::ThreadArg) -> usize {
    let cpu = cpu_id(3);
    let _execution = WorkerExecutionGuard::acquire(&CPU3_A_EXECUTION, cpu, "late-remote-ready");
    CPU3_A_RUNNING.store(1, Ordering::Release);
    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    while CPU3_B_PROGRESS.load(Ordering::Acquire) == 0 {
        assert_current_cpu_home(cpu, "late-remote-ready");
        if time::uptime_ms() >= deadline {
            protocol::fail("late-remote-ready", "REMOTE_READY_TIMEOUT");
        }
        core::hint::spin_loop();
    }
    0
}

fn cpu3_remote_b(_arg: sched::ThreadArg) -> usize {
    let cpu = cpu_id(3);
    let _execution = WorkerExecutionGuard::acquire(&CPU3_B_EXECUTION, cpu, "late-remote-ready");
    CPU3_B_PROGRESS.fetch_add(1, Ordering::AcqRel);
    0
}

fn spawn_on(cpu: cpu::CpuId, entry: sched::ThreadEntry, case: &'static str) -> sched::ThreadId {
    sched::thread_spawn(
        entry,
        sched::ThreadArg::from_usize(cpu.index()),
        sched::ThreadAttrs::joinable().with_affinity(cpu),
    )
    .unwrap_or_else(|_| protocol::fail(case, "SPAWN"))
}

fn join_worker(worker: sched::ThreadId, case: &'static str, reason: &str) {
    if sched::thread_join(worker).is_err() {
        protocol::fail(case, reason);
    }
    assert_current_cpu_home(cpu_id(0), case);
}

fn wait_for_cpu1_rr_workers() -> (sched::ThreadId, sched::ThreadId) {
    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    loop {
        if let Some(workers) = *CPU1_RR_WORKERS.lock() {
            return workers;
        }
        if time::uptime_ms() >= deadline {
            protocol::fail("cpu1-timer-round-robin", "LAUNCH_TIMEOUT");
        }
        assert_current_cpu_home(cpu_id(0), "cpu1-timer-round-robin");
        core::hint::spin_loop();
    }
}

fn wait_for_progress(progress: &AtomicUsize, case: &'static str, timeout_reason: &'static str) {
    let deadline = time::uptime_ms().saturating_add(WORKER_TIMEOUT_MS);
    while progress.load(Ordering::Acquire) == 0 {
        if time::uptime_ms() >= deadline {
            protocol::fail(case, timeout_reason);
        }
        assert_current_cpu_home(cpu_id(0), case);
        core::hint::spin_loop();
    }
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
