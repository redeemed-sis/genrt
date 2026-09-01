use super::{
    Result, SchedError, Scheduler, THREAD_STACK_SIZE, current_cpu, scheduler_slot_mut, thread,
    try_scheduler_mut,
};
use crate::{cpu, sync::LocalIrqGuard};

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
/// Returns `Ok(())` after publishing bounded scheduler state. Generic boot
/// publishes per-CPU time queues separately; architecture entry code owns
/// CPU-local interrupt initialization.
///
/// # Errors
///
/// Returns [`SchedError::AlreadyBootstrapped`] when scheduler state already
/// exists or [`SchedError::ThreadCapacityTooSmall`] when capacity cannot hold
/// one idle thread for every expected CPU plus every static thread.
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

    // Publish scheduler state before generic boot publishes deadline queues.
    // Timer dispatch calls the scheduler facade directly and therefore
    // requires the shared lifecycle table to exist first.
    let scheduler = Scheduler::bootstrap_new(
        cpu,
        idle_entry,
        idle_arg,
        threads,
        rr_quantum_ms,
        thread_capacity,
    )?;
    crate::sync::preempt::assert_pre_scheduler_state_quiescent(cpu);
    *scheduler_slot_mut() = Some(scheduler);
    Ok(())
}

/// Initialize the executing registered secondary CPU's scheduler context.
///
/// CPU0 must already have published the shared scheduler table, preallocated a
/// free slot for every expected CPU idle thread, and published this CPU's time
/// queue. The transition consumes exactly one free slot, publishes a permanent
/// nonjoinable idle thread with immutable local affinity, selects it as the
/// initial current thread, and leaves the context offline until first entry.
/// The operation is bounded, allocation-free, and does not enable IRQs.
///
/// # Arguments
///
/// * `idle_entry` - Permanent idle-thread entry for the executing CPU.
/// * `idle_arg` - Value passed to `idle_entry` on first execution.
///
/// # Returns
///
/// Returns `true` after the current registered secondary owns initialized
/// idle/current scheduler state. Returns `false` when scheduler state is not
/// published, the CPU is already initialized, or the preallocated free-slot
/// reserve was exhausted.
///
/// # Panics
///
/// Panics when current-CPU resolution, local preemption state, or scheduler
/// ownership violates the secondary entry contract.
pub(crate) fn initialize_current_cpu(
    idle_entry: thread::ThreadEntry,
    idle_arg: thread::ThreadArg,
) -> bool {
    let cpu = current_cpu();
    if cpu.index() == 0 || !cpu::is_registered(cpu) {
        return false;
    }
    crate::sync::preempt::assert_pre_scheduler_state_quiescent(cpu);
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let Some(mut scheduler) = try_scheduler_mut() else {
        return false;
    };

    scheduler
        .on_cpu(cpu)
        .transition_initialize_idle_current(idle_entry, idle_arg)
        .is_ok()
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
        let bootstrap_thread_count =
            required_bootstrap_slots(threads.len(), cpu::expected_count())?;
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

fn required_bootstrap_slots(
    static_thread_count: usize,
    expected_cpu_count: usize,
) -> Result<usize> {
    static_thread_count
        .checked_add(expected_cpu_count)
        .ok_or(SchedError::ThreadCapacityTooSmall)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_capacity_reserves_an_idle_slot_for_each_expected_cpu() {
        assert_eq!(required_bootstrap_slots(1, 4), Ok(5));
        assert_eq!(required_bootstrap_slots(8, 4), Ok(12));
    }

    #[test]
    fn bootstrap_capacity_rejects_overflow() {
        assert_eq!(
            required_bootstrap_slots(usize::MAX, 1),
            Err(SchedError::ThreadCapacityTooSmall)
        );
    }
}
