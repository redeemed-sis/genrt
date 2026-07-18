use core::cell::UnsafeCell;

use crate::{
    config::KERNEL_THREAD_CAPACITY,
    fs::fd::FdTable,
    loader::elf::UserElfImage,
    memory::vm::OwnedUserAddressSpace,
    sched::{self, ThreadId},
    sync::LocalIrqGuard,
};

use super::{
    error::ProcessError,
    id::{MAX_PROCESSES, ProcessId},
    record::Process,
    resources::ProcessImageResources,
    state::ProcessState,
};

const INITIAL_PROCESS_GENERATION: u32 = 1;

pub(super) struct ProcessSlot {
    pub(super) generation: u32,
    pub(super) process: Process,
}

impl ProcessSlot {
    pub(super) const fn free() -> Self {
        Self {
            generation: 0,
            process: Process::free(),
        }
    }

    pub(super) fn is_free(&self) -> bool {
        self.process.is_free()
    }
}

pub(super) struct ProcessTable {
    pub(super) slots: [ProcessSlot; MAX_PROCESSES],
    // O(1) current-process lookup keyed by `ThreadId::index`. An entry is
    // authoritative only when the referenced process slot still records the
    // exact generation-bearing `ThreadId` as its main thread.
    thread_owners: [Option<ProcessId>; KERNEL_THREAD_CAPACITY],
}

impl ProcessTable {
    pub(super) const fn new() -> Self {
        Self {
            slots: [const { ProcessSlot::free() }; MAX_PROCESSES],
            thread_owners: [None; KERNEL_THREAD_CAPACITY],
        }
    }

    pub(super) fn slot(&self, pid: ProcessId) -> Option<&ProcessSlot> {
        let slot = self.slots.get(pid.index())?;
        (slot.generation == pid.generation() && !slot.is_free()).then_some(slot)
    }

    pub(super) fn slot_mut(&mut self, pid: ProcessId) -> Option<&mut ProcessSlot> {
        let slot = self.slots.get_mut(pid.index())?;
        (slot.generation == pid.generation() && !slot.is_free()).then_some(slot)
    }

    pub(super) fn process_for_thread(&self, thread: ThreadId) -> Option<ProcessId> {
        let pid = *self.thread_owners.get(thread.index())?.as_ref()?;
        let slot = self
            .slot(pid)
            .unwrap_or_else(|| panic!("process: stale reverse owner {pid} for thread {thread}"));
        if slot.process.main_thread != Some(thread) {
            panic!("process: reverse owner mismatch pid={pid} thread={thread}");
        }
        Some(pid)
    }

    pub(super) fn bind_main_thread(&mut self, pid: ProcessId, thread: ThreadId) {
        if thread.index() >= self.thread_owners.len() {
            panic!("process: main thread index outside reverse owner table: {thread}");
        }
        if self.thread_owners[thread.index()].is_some() {
            panic!("process: main thread already belongs to a process: {thread}");
        }
        let slot = self
            .slot_mut(pid)
            .unwrap_or_else(|| panic!("process: published thread lost slot {pid}"));
        if slot.process.main_thread.is_some() {
            panic!("process: pid={pid} already has a main thread");
        }
        slot.process.main_thread = Some(thread);
        self.thread_owners[thread.index()] = Some(pid);
    }

    pub(super) fn unbind_main_thread(&mut self, pid: ProcessId) {
        let thread = self
            .slot(pid)
            .unwrap_or_else(|| panic!("process: unbind of invalid slot {pid}"))
            .process
            .main_thread
            .unwrap_or_else(|| panic!("process: pid={pid} has no main thread to unbind"));
        let owner = self
            .thread_owners
            .get_mut(thread.index())
            .unwrap_or_else(|| panic!("process: main thread index outside reverse owner table"));
        if *owner != Some(pid) {
            panic!("process: reverse owner missing during unbind pid={pid} thread={thread}");
        }
        *owner = None;
    }

    pub(super) fn release_slot(&mut self, pid: ProcessId, generation: u32) {
        let main_thread = self
            .slot(pid)
            .unwrap_or_else(|| panic!("process: release of invalid slot {pid}"))
            .process
            .main_thread;
        if let Some(thread) = main_thread {
            let owner = self
                .thread_owners
                .get_mut(thread.index())
                .unwrap_or_else(|| {
                    panic!("process: main thread index outside reverse owner table")
                });
            // A terminal process unbinds before generic thread reap. Its slot
            // may therefore have been reused and rebound before this later
            // process reclaim; never erase the newer generation's owner.
            if *owner == Some(pid) {
                *owner = None;
            }
        }
        self.slots[pid.index()] = ProcessSlot {
            generation,
            ..ProcessSlot::free()
        };
    }
}

struct ProcessTableCell(UnsafeCell<ProcessTable>);

// SAFETY: genrt is single-core for this milestone. Process table mutations run
// either in short IRQ-disabled thread-context sections or in the synchronous
// lower-EL trap path for the currently running thread.
unsafe impl Sync for ProcessTableCell {}

static PROCESS_TABLE: ProcessTableCell = ProcessTableCell(UnsafeCell::new(ProcessTable::new()));

pub(super) fn current_process_id() -> Option<ProcessId> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let thread = sched::current_thread_id()?;
    table_mut().process_for_thread(thread)
}

pub(super) fn allocate_process_slot(
    parent: Option<ProcessId>,
    fds: FdTable,
    cwd_dir: usize,
) -> Result<ProcessId, ProcessError> {
    if crate::fs::ramfs::dir_path(cwd_dir).is_none() {
        return Err(ProcessError::InvalidProcess);
    }
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let Some(index) = table
        .slots
        .iter()
        .enumerate()
        .find_map(|(index, slot)| slot.is_free().then_some(index))
    else {
        return Err(ProcessError::NoProcessSlots);
    };
    let generation = next_generation(table.slots[index].generation);
    table.slots[index] = ProcessSlot {
        generation,
        process: Process::running(parent, fds, cwd_dir),
    };
    let pid = ProcessId::new(index, generation);
    crate::debug!("process: allocated slot pid={pid}");
    Ok(pid)
}

pub(super) fn attach_process_resources(
    pid: ProcessId,
    address_space: OwnedUserAddressSpace,
    user_image: UserElfImage,
) {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table
        .slot_mut(pid)
        .unwrap_or_else(|| panic!("process: reserved slot disappeared before resource attach"));
    slot.process.resources.image.address_space = Some(address_space);
    slot.process.resources.image.user_image = Some(user_image);
    slot.process.state = ProcessState::Running;
    crate::debug!("process: pid={pid} resources ready");
}

pub(super) fn attach_process_main_thread(pid: ProcessId, main_thread: ThreadId) {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    table_mut().bind_main_thread(pid, main_thread);
    crate::debug!("process: pid={pid} running main={main_thread}");
}

pub(super) fn take_process_image_resources(pid: ProcessId) -> ProcessImageResources {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let Some(slot) = table.slot_mut(pid) else {
        return ProcessImageResources::empty();
    };
    ProcessImageResources {
        address_space: slot.process.resources.image.address_space.take(),
        user_image: slot.process.resources.image.user_image.take(),
    }
}

pub(super) fn free_process_slot(pid: ProcessId) {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    if let Some(generation) = table.slot(pid).map(|slot| slot.generation) {
        table.release_slot(pid, generation);
    }
}

pub(super) fn next_generation(generation: u32) -> u32 {
    let next = generation.wrapping_add(1);
    if next == 0 {
        INITIAL_PROCESS_GENERATION
    } else {
        next
    }
}

pub(super) fn table_mut() -> &'static mut ProcessTable {
    // SAFETY: single-core access discipline is documented on `ProcessTableCell`.
    unsafe { &mut *PROCESS_TABLE.0.get() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occupy_process_slot(table: &mut ProcessTable, index: usize, generation: u32) -> ProcessId {
        let pid = ProcessId::new(index, generation);
        table.slots[index] = ProcessSlot {
            generation,
            process: Process {
                state: ProcessState::Running,
                ..Process::free()
            },
        };
        pid
    }

    #[test]
    fn reverse_thread_owner_survives_thread_slot_reuse_before_process_reclaim() {
        let mut table = ProcessTable::new();
        let old_pid = occupy_process_slot(&mut table, 0, INITIAL_PROCESS_GENERATION);
        let old_thread = ThreadId::new(KERNEL_THREAD_CAPACITY - 1, 7);
        table.bind_main_thread(old_pid, old_thread);
        assert_eq!(table.process_for_thread(old_thread), Some(old_pid));
        assert_eq!(
            table.process_for_thread(ThreadId::new(old_thread.index() - 1, 7)),
            None
        );
        table.unbind_main_thread(old_pid);
        assert_eq!(table.process_for_thread(old_thread), None);
        let new_pid = occupy_process_slot(&mut table, 1, INITIAL_PROCESS_GENERATION);
        let new_thread = ThreadId::new(old_thread.index(), old_thread.generation() + 1);
        table.bind_main_thread(new_pid, new_thread);
        table.release_slot(old_pid, next_generation(old_pid.generation()));
        assert_eq!(table.process_for_thread(new_thread), Some(new_pid));
    }
}
