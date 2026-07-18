use crate::{
    fs::fd::FdTable,
    sched::{ThreadId, WaitToken},
};

use super::{
    files::ProcessFileState,
    id::ProcessId,
    resources::ProcessResources,
    state::{ProcessExitStatus, ProcessState},
};

/// Identity-independent metadata and resources for one process slot.
pub(super) struct Process {
    pub(super) state: ProcessState,
    pub(super) resources: ProcessResources,
    pub(super) parent: Option<ProcessId>,
    pub(super) main_thread: Option<ThreadId>,
    pub(super) exit_status: Option<ProcessExitStatus>,
    pub(super) process_consumer: Option<ThreadId>,
    pub(super) waiter: Option<WaitToken>,
}

impl Process {
    pub(super) const fn free() -> Self {
        Self {
            state: ProcessState::Free,
            resources: ProcessResources::free(),
            parent: None,
            main_thread: None,
            exit_status: None,
            process_consumer: None,
            waiter: None,
        }
    }

    pub(super) const fn running(parent: Option<ProcessId>, fds: FdTable, cwd_dir: usize) -> Self {
        Self {
            state: ProcessState::Running,
            resources: ProcessResources::new(ProcessFileState::new(fds, cwd_dir)),
            parent,
            main_thread: None,
            exit_status: None,
            process_consumer: None,
            waiter: None,
        }
    }

    pub(super) fn is_free(&self) -> bool {
        self.state == ProcessState::Free
    }
}
