use crate::{
    arch::ActiveContext,
    loader::elf,
    memory::vm,
    sched::{self, WaitCause, WaitToken},
    sync::LocalIrqGuard,
};

use super::{
    error::{ProcessFaultError, ProcessJoinError},
    id::ProcessId,
    resources::ProcessImageResources,
    state::{ProcessExitStatus, ProcessState},
    table::{current_process_id, next_generation, table_mut},
};

struct ReclaimedProcess {
    status: ProcessExitStatus,
    resources: ProcessImageResources,
}

pub(crate) fn process_join(pid: ProcessId) -> Result<ProcessExitStatus, ProcessJoinError> {
    if current_process_id() == Some(pid) {
        return Err(ProcessJoinError::SelfJoin);
    }
    let caller = sched::current_thread_id().ok_or(ProcessJoinError::SchedulerNotInitialized)?;
    let main_thread = claim_process_consumer(pid, caller)?;
    sched::thread_join(main_thread).map_err(|error| match error {
        sched::JoinError::JoinInProgress => ProcessJoinError::JoinInProgress,
        sched::JoinError::SchedulerNotInitialized => ProcessJoinError::SchedulerNotInitialized,
        _ => ProcessJoinError::InvalidProcess,
    })?;
    let reclaimed = match consume_process_for_join(pid, caller) {
        Ok(reclaimed) => reclaimed,
        Err(ConsumeProcessError::WouldBlock) => return Err(ProcessJoinError::InvalidProcess),
        Err(ConsumeProcessError::Join(err)) => return Err(err),
    };
    cleanup_reclaimed_process(pid, reclaimed)
}

/// Store normal process exit status and switch away from its current thread.
///
/// # Arguments
///
/// * `context` - Exclusive live userspace context replaced by scheduler exit.
/// * `code` - Userspace exit code stored in process and thread state.
///
/// # Returns
///
/// Returns only in the exception handler after the live frame has been replaced
/// with another runnable thread; the exiting userspace thread does not resume.
/// The terminal transition is bounded and does not allocate.
///
/// # Panics
///
/// Panics if no current process can own the exit or a
/// [`crate::sync::preempt::PreemptGuard`] is active before terminal state changes.
pub(crate) fn process_exit_current(context: &mut ActiveContext<'_>, code: usize) {
    crate::sync::preempt::assert_preemption_enabled("process exit state change");
    finish_current_process(context, ProcessExitStatus::Exited(code), code)
        .unwrap_or_else(|err| panic!("process: sys_exit without current process: {err:?}"));
}

pub(super) fn finish_current_process(
    context: &mut ActiveContext<'_>,
    status: ProcessExitStatus,
    thread_code: usize,
) -> Result<(), ProcessFaultError> {
    crate::sync::preempt::assert_preemption_enabled("process terminal state change");
    let pid = current_process_id().ok_or(ProcessFaultError::NoCurrentProcess)?;
    let thread = sched::current_thread_id();
    let wake = {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        finish_process(pid, status).ok_or(ProcessFaultError::InvalidProcess)?
    };
    match status {
        ProcessExitStatus::Exited(code) => {
            crate::debug!("process: pid={pid} exited code={code} thread={thread:?}")
        }
        ProcessExitStatus::Faulted(fault) => crate::warn!(
            "process: pid={pid} faulted kind={:?} thread={thread:?}",
            fault.kind
        ),
    }
    if let Some(waiter) = wake {
        let _ = sched::complete_wait(waiter, WaitCause::Notified);
    }
    sched::on_thread_exit_sync(context, thread_code);
    Ok(())
}

fn finish_process(pid: ProcessId, status: ProcessExitStatus) -> Option<Option<WaitToken>> {
    let table = table_mut();
    let waiter = {
        let slot = table.slot_mut(pid)?;
        if slot.process.exit_status.is_some() {
            return Some(None);
        }
        slot.process.resources.files.close_all();
        slot.process.state = ProcessState::Zombie;
        slot.process.exit_status = Some(status);
        slot.process.waiter
    };
    table.unbind_main_thread(pid);
    Some(waiter)
}

fn claim_process_consumer(
    pid: ProcessId,
    caller: crate::sched::ThreadId,
) -> Result<crate::sched::ThreadId, ProcessJoinError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let slot = table_mut()
        .slot_mut(pid)
        .ok_or(ProcessJoinError::InvalidProcess)?;
    if slot.process.process_consumer.is_some() {
        return Err(ProcessJoinError::JoinInProgress);
    }
    let main_thread = slot
        .process
        .main_thread
        .ok_or(ProcessJoinError::InvalidProcess)?;
    slot.process.process_consumer = Some(caller);
    Ok(main_thread)
}

enum ConsumeProcessError {
    WouldBlock,
    Join(ProcessJoinError),
}

fn consume_process_for_join(
    pid: ProcessId,
    caller: crate::sched::ThreadId,
) -> Result<ReclaimedProcess, ConsumeProcessError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table
        .slot_mut(pid)
        .ok_or(ConsumeProcessError::Join(ProcessJoinError::InvalidProcess))?;
    if slot.process.process_consumer != Some(caller) {
        return Err(ConsumeProcessError::Join(ProcessJoinError::JoinInProgress));
    }
    let Some(status) = slot.process.exit_status.take() else {
        return Err(ConsumeProcessError::WouldBlock);
    };
    let reclaimed = ReclaimedProcess {
        status,
        resources: ProcessImageResources {
            address_space: slot.process.resources.image.address_space.take(),
            user_image: slot.process.resources.image.user_image.take(),
        },
    };
    let next_generation = next_generation(slot.generation);
    table.release_slot(pid, next_generation);
    Ok(reclaimed)
}

fn cleanup_reclaimed_process(
    pid: ProcessId,
    reclaimed: ReclaimedProcess,
) -> Result<ProcessExitStatus, ProcessJoinError> {
    let status = reclaimed.status;
    if let Some(image) = reclaimed.resources.user_image {
        elf::free_loaded_segments(&image);
    }
    if let Some(address_space) = reclaimed.resources.address_space {
        // SAFETY: join reaped the user thread and released its thread-owned stack before this process resource cleanup.
        if let Err(err) = unsafe { vm::destroy_user_address_space(address_space) } {
            crate::warn!("process: failed to destroy address space pid={pid}: {err:?}");
        }
    }
    crate::debug!("process: reclaimed pid={pid} status={status:?}");
    Ok(status)
}
