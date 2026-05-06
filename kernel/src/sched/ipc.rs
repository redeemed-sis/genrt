use crate::{
    ipc::{IpcWaitRegistration, IpcWaitToken},
    sync::LocalIrqGuard,
    task::TaskId,
    time::TimedEvent,
};

use super::{Scheduler, preempt::BlockReason, preempt::TaskState, scheduler_mut};

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

    fn set_wait_result(&mut self, result: WaitResult) {
        self.last_wait_result = Some(result);
    }

    fn take_wait_result(&mut self) -> Option<WaitResult> {
        self.last_wait_result.take()
    }
}

pub(super) fn on_ipc_timeout(task_id: TaskId) {
    scheduler_mut().handle_ipc_timeout(task_id);
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

pub(crate) fn block_current_on_ipc(active_frame_words: *mut u64, wait: IpcWaitRegistration) {
    scheduler_mut().block_current_on_ipc(active_frame_words, wait);
}

impl Scheduler {
    fn block_current_on_ipc(&mut self, active_frame_words: *mut u64, wait: IpcWaitRegistration) {
        let current = self.blocking_current(active_frame_words);
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

        let next = self.block_current_with_reason(active_frame_words, current, reason);
        self.finish_block_current(current, next);
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

        let timeout_event = match self.task(task_id).state {
            TaskState::Blocked(BlockReason::Ipc(wait)) => wait.timeout_event,
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

        if let Some(event) = timeout_event {
            crate::time::cancel_event(event);
            crate::debug!("sched: normal IPC wake canceled timeout {event:?}");
        }

        self.task_mut(task_id)
            .ipc
            .set_wait_result(WaitResult::Completed);
        self.wake_task(task_id);
    }

    fn handle_ipc_timeout(&mut self, task_id: TaskId) {
        if !self.is_valid_task(task_id) {
            return;
        }

        let wait = match self.task(task_id).state {
            TaskState::Blocked(BlockReason::Ipc(wait))
                if wait.timeout_event == Some(TimedEvent::IpcTimeout(task_id)) =>
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

        self.task_mut(task_id)
            .ipc
            .set_wait_result(WaitResult::TimedOut);
        self.wake_task(task_id);
        crate::debug!(
            "sched: IPC timeout completed task {task_id} token={:?}",
            wait.token
        );
    }

    fn clear_current_wait_result(&mut self) {
        if let Some(current) = self.current {
            self.task_mut(current).ipc.reset();
        }
    }

    fn set_current_wait_result(&mut self, result: WaitResult) {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("sched: wait result without running task"));
        self.task_mut(current).ipc.set_wait_result(result);
    }

    fn take_current_wait_result(&mut self) -> Option<WaitResult> {
        let current = self
            .running_task()
            .unwrap_or_else(|| panic!("sched: wait result without running task"));
        self.task_mut(current).ipc.take_wait_result()
    }
}
