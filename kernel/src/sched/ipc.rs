use crate::{
    arch::ActiveContext,
    ipc::{IpcWaitRegistration, IpcWaitToken},
    sync::LocalIrqGuard,
    task::{TaskId, ThreadId},
    time::TimedEvent,
};

use super::{
    Scheduler, scheduler_mut,
    transition::{BlockReason, TaskState},
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum WaitResult {
    Completed,
    TimedOut,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct IpcBlock {
    pub(super) token: IpcWaitToken,
    pub(super) timeout_event: Option<TimedEvent>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct TaskIpcState {
    last_wait_result: Option<WaitResult>,
}

impl TaskIpcState {
    pub(super) const fn empty() -> Self {
        Self {
            last_wait_result: None,
        }
    }

    pub(super) fn reset(&mut self) {
        self.last_wait_result = None;
    }

    pub(super) fn is_empty(&self) -> bool {
        self.last_wait_result.is_none()
    }

    pub(super) fn set_wait_result(&mut self, result: WaitResult) {
        self.last_wait_result = Some(result);
    }

    pub(super) fn take_wait_result(&mut self) -> Option<WaitResult> {
        self.last_wait_result.take()
    }
}

pub(super) fn on_ipc_timeout(thread: ThreadId) {
    scheduler_mut().handle_ipc_timeout(thread);
}

pub(crate) fn complete_ipc_wait(task_id: TaskId) {
    scheduler_mut().complete_ipc_wait(task_id);
}

pub(crate) fn clear_current_wait_result() {
    scheduler_mut().clear_current_wait_result();
}

pub(crate) fn set_current_wait_result(result: WaitResult) {
    scheduler_mut().set_current_wait_result(result);
}

pub(crate) fn take_current_wait_result() -> Option<WaitResult> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    scheduler_mut().take_current_wait_result()
}

/// Commit the current task to a registered IPC wait.
///
/// # Arguments
///
/// * `context` - Exclusive live task-call context saved and replaced by the
///   scheduler.
/// * `wait` - Owning IPC token and optional prevalidated timeout deadline.
///
/// # Returns
///
/// Returns after the task is later resumed. Timeout and scheduler queues are
/// preallocated, so the handoff does not allocate.
///
/// # Panics
///
/// Panics for missing running-task state or exhausted bounded event capacity.
pub(crate) fn block_current_on_ipc(context: &mut ActiveContext<'_>, wait: IpcWaitRegistration) {
    scheduler_mut().block_current_on_ipc(context, wait);
}

impl Scheduler {
    fn block_current_on_ipc(&mut self, context: &mut ActiveContext<'_>, wait: IpcWaitRegistration) {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("sched: IPC wait without running task"));
        let timeout_event = wait.timeout_deadline().map(|deadline| {
            let event = TimedEvent::IpcTimeout(current);
            crate::time::schedule_event(deadline, event);
            crate::debug!("sched: timeout event scheduled {event:?} deadline={deadline}");
            event
        });
        let reason = BlockReason::Ipc(IpcBlock {
            token: wait.token(),
            timeout_event,
        });

        let (from, next) = self.begin_block_current(context, reason);
        debug_assert_eq!(from, current);
        self.finish_block_current(from, next);
        crate::trace!(
            "sched: task {current} blocked on IPC token={:?} timeout_event={:?}",
            wait.token(),
            timeout_event
        );
    }

    fn complete_ipc_wait(&mut self, task_id: TaskId) {
        if !self.is_valid_task(task_id) {
            return;
        }

        let wait = match self.task_state(task_id) {
            TaskState::Blocked(BlockReason::Ipc(wait)) => wait,
            TaskState::Blocked(reason) => {
                crate::trace!(
                    "sched: ignoring IPC completion for task {task_id}; blocked on {reason:?}"
                );
                return;
            }
            state => {
                crate::trace!("sched: ignoring IPC completion for task {task_id}; state={state:?}");
                return;
            }
        };

        if let Some(event) = wait.timeout_event {
            crate::time::cancel_event(event);
            crate::debug!("sched: normal IPC wake canceled timeout {event:?}");
        }

        self.ipc_mut(task_id).set_wait_result(WaitResult::Completed);
        let thread = self.thread_id(task_id);
        if !self.transition_wake_thread(thread, BlockReason::Ipc(wait)) {
            panic!("sched: IPC completion failed to wake {thread}");
        }
    }

    fn handle_ipc_timeout(&mut self, thread: ThreadId) {
        if !self.thread_matches(thread) {
            return;
        }
        let task_id = TaskId::new(thread.index());

        let wait = match self.task_state(task_id) {
            TaskState::Blocked(BlockReason::Ipc(wait))
                if wait.timeout_event == Some(TimedEvent::IpcTimeout(thread)) =>
            {
                wait
            }
            TaskState::Blocked(BlockReason::Ipc(wait)) => {
                crate::trace!("sched: stale IPC timeout for task {task_id}; current wait={wait:?}");
                return;
            }
            state => {
                crate::trace!("sched: stale IPC timeout for task {task_id}; state={state:?}");
                return;
            }
        };

        let removed = crate::ipc::remove_timed_out_waiter(wait.token, task_id);
        if !removed {
            panic!("sched: IPC timeout task {task_id} missing from IPC wait queue");
        }

        self.ipc_mut(task_id).set_wait_result(WaitResult::TimedOut);
        if !self.transition_wake_thread(thread, BlockReason::Ipc(wait)) {
            panic!("sched: IPC timeout failed to wake {thread}");
        }
        crate::debug!(
            "sched: IPC timeout completed task {task_id} token={:?}",
            wait.token
        );
    }

    fn clear_current_wait_result(&mut self) {
        if let Some(current) = self.running_task() {
            self.ipc_mut(TaskId::new(current.index())).reset();
        }
    }

    fn set_current_wait_result(&mut self, result: WaitResult) {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("sched: wait result without running task"));
        self.ipc_mut(TaskId::new(current.index()))
            .set_wait_result(result);
    }

    fn take_current_wait_result(&mut self) -> Option<WaitResult> {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("sched: wait result without running task"));
        self.ipc_mut(TaskId::new(current.index()))
            .take_wait_result()
    }
}
