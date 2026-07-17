use crate::time::TimeHandlers;

use super::{
    Result, SchedError, Scheduler, THREAD_STACK_SIZE, ipc as sched_ipc, preempt,
    scheduler_slot_mut, thread,
};

#[derive(Copy, Clone)]
/// Immutable entry descriptor for a kernel task created during bootstrap.
pub(crate) struct StaticTask {
    entry: thread::ThreadEntry,
    arg: thread::ThreadArg,
}

impl StaticTask {
    /// Create a static bootstrap task descriptor.
    ///
    /// This constructor records task metadata only. Stack/frame allocation and
    /// ready-queue insertion occur later during scheduler bootstrap.
    ///
    /// # Arguments
    ///
    /// - `entry`: Kernel thread entry function invoked by the bootstrap frame.
    /// - `arg`: Value passed to `entry` when the task first runs.
    ///
    /// # Returns
    ///
    /// Returns an immutable descriptor containing `entry` and `arg`.
    pub(crate) const fn new(entry: thread::ThreadEntry, arg: thread::ThreadArg) -> Self {
        Self { entry, arg }
    }
}

pub(crate) fn bootstrap(
    idle_entry: thread::ThreadEntry,
    idle_arg: thread::ThreadArg,
    tasks: &[StaticTask],
    rr_quantum_ms: u64,
    thread_capacity: usize,
) -> Result<()> {
    if scheduler_slot_mut().is_some() {
        return Err(SchedError::AlreadyBootstrapped);
    }

    // Bootstrap ordering is intentionally split into two phases:
    // 1. Build and publish the global scheduler so timer-owned callbacks always
    //    have a concrete scheduler instance to reach.
    // 2. Only then initialize `kernel::time`, which is the first point where
    //    timer IRQ dispatch can invoke scheduler callbacks.
    let scheduler =
        Scheduler::bootstrap_new(idle_entry, idle_arg, tasks, rr_quantum_ms, thread_capacity)?;
    let task_count = scheduler.transition_task_count();
    *scheduler_slot_mut() = Some(scheduler);
    init_time_after_scheduler_publish(task_count);
    Ok(())
}

#[inline(always)]
fn init_time_after_scheduler_publish(task_count: usize) {
    crate::time::init(
        TimeHandlers {
            finish_timer_interrupt: preempt::finish_timer_interrupt,
            wake_task: preempt::on_wake_thread,
            quantum_expired: preempt::on_quantum_expired,
            ipc_timeout: sched_ipc::on_ipc_timeout,
        },
        task_count.saturating_mul(crate::time::TIMED_EVENT_CAPACITY_PER_TASK),
    );
}

impl Scheduler {
    fn bootstrap_new(
        idle_entry: thread::ThreadEntry,
        idle_arg: thread::ThreadArg,
        tasks: &[StaticTask],
        rr_quantum_ms: u64,
        thread_capacity: usize,
    ) -> Result<Self> {
        let bootstrap_task_count = tasks.len() + 1;
        if thread_capacity < bootstrap_task_count {
            return Err(SchedError::ThreadCapacityTooSmall);
        }

        let mut scheduler = Self::transition_new(thread_capacity, rr_quantum_ms);
        scheduler.transition_append_bootstrap(idle_entry, idle_arg, true);
        for task in tasks {
            let id = scheduler.transition_append_bootstrap(task.entry, task.arg, false);
            crate::debug!("sched: bootstrap task {id}");
        }
        scheduler.transition_fill_free_slots(thread_capacity);

        scheduler.transition_initial_dispatch()?;
        crate::debug!(
            "sched: thread_capacity={} ready_queue_capacity={} stack_size={} quantum={}ms",
            scheduler.transition_task_count(),
            scheduler.transition_ready_capacity(),
            THREAD_STACK_SIZE,
            scheduler.rr_quantum_ms
        );
        Ok(scheduler)
    }
}
