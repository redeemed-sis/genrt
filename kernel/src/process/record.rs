use crate::{
    cpu::CpuId,
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
    pub(super) owner_cpu: Option<CpuId>,
    pub(super) pending_fork_cpu: Option<CpuId>,
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
            owner_cpu: None,
            pending_fork_cpu: None,
            resources: ProcessResources::free(),
            parent: None,
            main_thread: None,
            exit_status: None,
            process_consumer: None,
            waiter: None,
        }
    }

    pub(super) const fn running(
        parent: Option<ProcessId>,
        fds: FdTable,
        cwd_dir: usize,
        owner_cpu: CpuId,
    ) -> Self {
        Self {
            state: ProcessState::Running,
            owner_cpu: Some(owner_cpu),
            pending_fork_cpu: None,
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

    pub(super) fn owner_cpu(&self) -> CpuId {
        self.owner_cpu
            .unwrap_or_else(|| panic!("process: live process lacks owner CPU"))
    }

    pub(super) fn set_pending_fork_cpu(&mut self, cpu: Option<CpuId>) {
        self.pending_fork_cpu = cpu;
    }

    pub(super) fn next_child_cpu(&self) -> CpuId {
        self.pending_fork_cpu.unwrap_or_else(|| self.owner_cpu())
    }

    pub(super) fn consume_pending_fork_cpu(&mut self) {
        self.pending_fork_cpu = None;
    }
}
