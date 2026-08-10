use super::{
    Result, SchedError, Scheduler, THREAD_STACK_SIZE, current_cpu, scheduler_slot_mut, thread,
};

#[derive(Copy, Clone)]
/// Immutable entry descriptor for a kernel thread created during bootstrap.
pub(crate) struct StaticThread {
    entry: thread::ThreadEntry,
    arg: thread::ThreadArg,
}

impl StaticThread {
    /// Create a static bootstrap thread descriptor.
    ///
    /// This constructor records thread metadata only. Stack/frame allocation and
    /// ready-queue insertion occur later during scheduler bootstrap.
    ///
    /// # Arguments
    ///
    /// * `entry` - Kernel thread entry function invoked by the bootstrap frame.
    /// * `arg` - Value passed to `entry` when the thread first runs.
    ///
    /// # Returns
    ///
    /// Returns an immutable descriptor containing `entry` and `arg`. This is a
    /// const metadata operation that does not allocate, block, or alter IRQ
    /// state.
    pub(crate) const fn new(entry: thread::ThreadEntry, arg: thread::ThreadArg) -> Self {
        Self { entry, arg }
    }
}

/// Build and publish the bounded scheduler bootstrap set.
///
/// Kernel-stack and ready-queue storage are preallocated before scheduler
/// entry. The function allocates only during bootstrap setup, before runtime
/// IRQ scheduling begins; it must not be called from IRQ context or block.
///
/// # Arguments
///
/// * `idle_entry` - Entry function for the mandatory idle thread.
/// * `idle_arg` - Argument supplied to `idle_entry`.
/// * `threads` - Additional static kernel-thread descriptors.
/// * `rr_quantum_ms` - Requested round-robin quantum in milliseconds.
/// * `thread_capacity` - Total bounded slot and preallocated stack capacity.
///
/// # Returns
///
/// Returns `Ok(())` after publishing the scheduler and initializing timer
/// callbacks that reference it.
///
/// # Errors
///
/// Returns [`SchedError::AlreadyBootstrapped`] when scheduler state already
/// exists or [`SchedError::ThreadCapacityTooSmall`] when capacity cannot hold
/// idle plus every static thread.
///
/// # Panics
///
/// Panics if bootstrap-time allocator or scheduler transition invariants fail.
pub(crate) fn bootstrap(
    idle_entry: thread::ThreadEntry,
    idle_arg: thread::ThreadArg,
    threads: &[StaticThread],
    rr_quantum_ms: u64,
    thread_capacity: usize,
) -> Result<()> {
    if scheduler_slot_mut().is_some() {
        return Err(SchedError::AlreadyBootstrapped);
    }
    let cpu = current_cpu();

    // Publish scheduler state before enabling this CPU's deadline queue. Timer
    // dispatch calls the scheduler facade directly and therefore requires the
    // shared lifecycle table to exist first.
    let scheduler = Scheduler::bootstrap_new(
        cpu,
        idle_entry,
        idle_arg,
        threads,
        rr_quantum_ms,
        thread_capacity,
    )?;
    let thread_count = scheduler.transition_thread_count();
    crate::sync::preempt::assert_pre_scheduler_state_quiescent(cpu);
    *scheduler_slot_mut() = Some(scheduler);
    init_time_after_scheduler_publish(thread_count);
    Ok(())
}

#[inline(always)]
fn init_time_after_scheduler_publish(thread_count: usize) {
    crate::time::init_current_cpu(
        thread_count.saturating_mul(crate::time::TIMED_EVENT_CAPACITY_PER_THREAD),
    );
}

impl Scheduler {
    fn bootstrap_new(
        cpu: crate::cpu::CpuId,
        idle_entry: thread::ThreadEntry,
        idle_arg: thread::ThreadArg,
        threads: &[StaticThread],
        rr_quantum_ms: u64,
        thread_capacity: usize,
    ) -> Result<Self> {
        let bootstrap_thread_count = threads.len() + 1;
        if thread_capacity < bootstrap_thread_count {
            return Err(SchedError::ThreadCapacityTooSmall);
        }

        let mut scheduler = Self::transition_new(thread_capacity, rr_quantum_ms);
        scheduler
            .on_cpu(cpu)
            .transition_append_bootstrap(idle_entry, idle_arg, true);
        for thread in threads {
            let id =
                scheduler
                    .on_cpu(cpu)
                    .transition_append_bootstrap(thread.entry, thread.arg, false);
            crate::debug!("sched: bootstrap thread {id}");
        }
        scheduler.transition_fill_free_slots(thread_capacity);

        scheduler.on_cpu(cpu).transition_initial_dispatch()?;
        crate::debug!(
            "sched: thread_capacity={} ready_queue_capacity={} stack_size={} quantum={}ms",
            scheduler.transition_thread_count(),
            scheduler.on_cpu(cpu).transition_ready_capacity(),
            THREAD_STACK_SIZE,
            scheduler.rr_quantum_ms
        );
        Ok(scheduler)
    }
}
