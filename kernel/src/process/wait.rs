use super::{
    error::wait_user_copy_errno,
    id::ProcessId,
    resources::{ProcessImageResources, cleanup_process_image_resources},
    state::ProcessExitStatus,
    table::{current_process_id, next_generation, table_mut},
};
use crate::{
    arch::ActiveContext,
    errno,
    memory::user,
    sched::{self, CommitResult, ThreadId, WaitToken},
    sync::LocalIrqGuard,
};
use core::mem;

struct WaitedProcess {
    pid: ProcessId,
    status: ProcessExitStatus,
    resources: ProcessImageResources,
    main_thread: ThreadId,
    wait_token: Option<WaitToken>,
}

pub(crate) enum WaitPidAction {
    Return(Result<usize, errno::Errno>),
    Blocked,
}
enum WaitChildPrepare {
    Consumed(WaitedProcess),
    Prepared(sched::PreparedWait),
    Err(errno::Errno),
}

/// Consume or block for one specific child process status.
///
/// The prepare/register/block transition runs in a short IRQ-disabled section
/// and does not allocate. Resource cleanup and userspace status copying occur
/// after that section.
///
/// # Arguments
///
/// * `context` - Exclusive live syscall context restarted and handed to the
///   scheduler when the child is still running.
/// * `raw_pid` - Positive generation-encoded child PID from userspace.
/// * `status_ptr` - Optional userspace pointer receiving encoded wait status.
///
/// # Returns
///
/// Returns [`WaitPidAction::Return`] with a PID or errno when complete, or
/// [`WaitPidAction::Blocked`] after registering a restartable wait.
pub(crate) fn waitpid_current(
    context: &mut ActiveContext<'_>,
    raw_pid: isize,
    status_ptr: usize,
) -> WaitPidAction {
    if raw_pid <= 0 {
        return WaitPidAction::Return(Err(errno::ECHILD));
    }
    let Some(child_pid) = ProcessId::from_raw(raw_pid as usize) else {
        return WaitPidAction::Return(Err(errno::ECHILD));
    };
    if status_ptr != 0
        && user::validate_user_write_range(status_ptr, mem::size_of::<u32>())
            .map_err(wait_user_copy_errno)
            .is_err()
    {
        return WaitPidAction::Return(Err(errno::EFAULT));
    }
    let parent_pid = match current_process_id() {
        Some(pid) => pid,
        None => return WaitPidAction::Return(Err(errno::EINVAL)),
    };
    let caller = match sched::current_thread_id() {
        Some(thread) => thread,
        None => return WaitPidAction::Return(Err(errno::EINVAL)),
    };
    let prepare = {
        let _irq_guard = LocalIrqGuard::save_and_disable();
        prepare_wait_child_locked(parent_pid, child_pid, caller)
    };
    match prepare {
        WaitChildPrepare::Consumed(waited) => {
            if let Some(token) = waited.wait_token {
                match sched::finish_wait(token) {
                    Ok(_) | Err(sched::FinishError::Stale) => {}
                    Err(sched::FinishError::NotCompleted) => {
                        panic!("waitpid: child wait not completed")
                    }
                }
            }
            let encoded = encode_wait_status(waited.status);
            if sched::thread_join(waited.main_thread).is_err() {
                return WaitPidAction::Return(Err(errno::ECHILD));
            }
            cleanup_waited_process(waited);
            if status_ptr != 0
                && user::copy_to_user(status_ptr, &encoded.to_le_bytes())
                    .map_err(wait_user_copy_errno)
                    .is_err()
            {
                return WaitPidAction::Return(Err(errno::EFAULT));
            }
            WaitPidAction::Return(Ok(child_pid.as_raw()))
        }
        WaitChildPrepare::Prepared(prepared) => {
            context.restart_current_syscall();
            match sched::commit_wait(context, prepared) {
                CommitResult::Blocked(_) | CommitResult::Early(_) => WaitPidAction::Blocked,
                CommitResult::Stale => panic!("waitpid: prepared wait became stale before commit"),
            }
        }
        WaitChildPrepare::Err(errno) => WaitPidAction::Return(Err(errno)),
    }
}

fn prepare_wait_child_locked(
    parent_pid: ProcessId,
    child_pid: ProcessId,
    caller: crate::sched::ThreadId,
) -> WaitChildPrepare {
    let table = table_mut();
    let Some(slot) = table.slot_mut(child_pid) else {
        return WaitChildPrepare::Err(errno::ECHILD);
    };
    if slot.process.parent != Some(parent_pid) {
        return WaitChildPrepare::Err(errno::ECHILD);
    }
    match slot.process.process_consumer {
        Some(consumer) if consumer != caller => return WaitChildPrepare::Err(errno::ECHILD),
        Some(_) => {}
        None => slot.process.process_consumer = Some(caller),
    }
    if let Some(waiter) = slot.process.waiter {
        if waiter.thread() != caller {
            return WaitChildPrepare::Err(errno::ECHILD);
        }
    }
    let Some(status) = slot.process.exit_status.take() else {
        if slot.process.waiter.is_some() {
            panic!("waitpid: current caller already owns an unfinished child wait");
        }
        crate::sync::preempt::assert_preemption_enabled("process waiter publication");
        let prepared = sched::prepare_wait();
        slot.process.waiter = Some(prepared.token());
        return WaitChildPrepare::Prepared(prepared);
    };
    let main_thread = slot
        .process
        .main_thread
        .unwrap_or_else(|| panic!("waitpid: terminal process has no main thread"));
    let waited = WaitedProcess {
        pid: child_pid,
        status,
        resources: ProcessImageResources {
            address_space: slot.process.resources.image.address_space.take(),
            user_image: slot.process.resources.image.user_image.take(),
        },
        main_thread,
        wait_token: slot.process.waiter.take(),
    };
    let next_generation = next_generation(slot.generation);
    table.release_slot(child_pid, next_generation);
    WaitChildPrepare::Consumed(waited)
}

fn cleanup_waited_process(waited: WaitedProcess) {
    cleanup_process_image_resources(waited.pid, waited.resources);
    crate::debug!(
        "waitpid: reclaimed child pid={} status={:?}",
        waited.pid,
        waited.status
    );
}
fn encode_wait_status(status: ProcessExitStatus) -> u32 {
    match status {
        ProcessExitStatus::Exited(code) => ((code as u32) & 0xff) << 8,
        ProcessExitStatus::Faulted(_) => 0x7f,
    }
}
