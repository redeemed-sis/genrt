//! Scheduler lifecycle transitions and bounded state validation.
//!
//! This module is the sole owner of thread lifecycle state, current-thread identity,
//! and ready-queue membership. Handoff code supplies architecture context save /
//! restore and address-space activation after a transition selects its outcome.

use alloc::{boxed::Box, collections::VecDeque, vec::Vec};

use crate::{
    arch::SavedContext,
    memory::{user::OwnedUserStack, vm::AddressSpaceId},
};

use super::{
    IDLE_THREAD_INDEX, INITIAL_THREAD_GENERATION, Scheduler, THREAD_STACK_SIZE, ThreadId, thread,
    wait::{
        CancelResult, CommitState, CompletionResult, FinishError, PreparedWait, WaitCause,
        WaitMetadata, WaitToken,
    },
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum ThreadState {
    Ready,
    Running,
    Blocked,
    Exited,
}

/// Resources that belong to a user thread rather than its process.
///
/// The address-space identifier is copyable scheduler metadata. The mapped
/// user stack is uniquely owned by the thread and is extracted before a
/// exited thread is released so its destructor never runs under an IRQ guard.
pub(crate) struct UserThreadResources {
    address_space: AddressSpaceId,
    stack: OwnedUserStack,
}

impl UserThreadResources {
    pub(super) fn new(address_space: AddressSpaceId, stack: OwnedUserStack) -> Self {
        Self {
            address_space,
            stack,
        }
    }

    pub(super) const fn address_space(&self) -> AddressSpaceId {
        self.address_space
    }

    pub(super) fn stack(&self) -> &OwnedUserStack {
        &self.stack
    }
}

/// A context-free scheduling decision for architecture handoff code.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum SwitchOutcome {
    ContinueCurrent,
    Switch { from: ThreadId, to: ThreadId },
}

#[repr(C, align(16))]
struct OwnedKernelStack {
    bytes: [u8; THREAD_STACK_SIZE],
}

impl OwnedKernelStack {
    fn top(&self) -> usize {
        self.bytes.as_ptr() as usize + THREAD_STACK_SIZE
    }
}

struct Thread {
    state: ThreadState,
    joinable: bool,
    exit_code: Option<usize>,
    joiner: Option<WaitToken>,
    last_join_result: Option<core::result::Result<usize, thread::JoinError>>,
    reaped_user: Option<UserThreadResources>,
    user: Option<UserThreadResources>,
    wait: WaitMetadata,
    stack: Box<OwnedKernelStack>,
    context: SavedContext,
}

struct ThreadSlot {
    generation: u32,
    next_wait_sequence: u64,
    stack: Option<Box<OwnedKernelStack>>,
    thread: Option<Thread>,
}

impl ThreadSlot {
    fn free() -> Self {
        Self {
            generation: 0,
            next_wait_sequence: 0,
            stack: Some(boxed_zeroed_stack()),
            thread: None,
        }
    }
}

pub(super) struct ThreadTable {
    slots: Vec<ThreadSlot>,
    free_slots: Vec<usize>,
    ready_queue: VecDeque<ThreadId>,
    current: Option<ThreadId>,
}

impl ThreadTable {
    fn with_capacity(thread_capacity: usize) -> Self {
        let mut slots = Vec::new();
        slots.reserve_exact(thread_capacity);
        let mut free_slots = Vec::new();
        free_slots.reserve_exact(thread_capacity);

        let mut ready_queue = VecDeque::new();
        ready_queue.reserve_exact(thread_capacity.saturating_sub(1));

        Self {
            slots,
            free_slots,
            ready_queue,
            current: None,
        }
    }

    /// Look up a live thread directly by its generation-bearing ID.
    fn get(&self, id: ThreadId) -> Option<&Thread> {
        let slot = self.slots.get(id.index())?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.thread.as_ref()
    }

    /// Mutably look up a live thread directly by its generation-bearing ID.
    fn get_mut(&mut self, id: ThreadId) -> Option<&mut Thread> {
        let slot = self.slots.get_mut(id.index())?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.thread.as_mut()
    }

    /// Release a non-current exited thread and return its user resources for cleanup.
    fn release(&mut self, id: ThreadId) -> Option<UserThreadResources> {
        let slot = self.slots.get_mut(id.index())?;
        if slot.generation != id.generation() {
            return None;
        }
        let mut thread = slot.thread.take()?;
        if !thread.wait.is_none() {
            panic!("sched: release of {id} with active wait metadata");
        }
        let user = thread.user.take();
        slot.next_wait_sequence = thread.wait.next_sequence();
        slot.stack = Some(thread.stack);
        self.free_slots.push(id.index());
        user
    }
}

fn boxed_zeroed_stack() -> Box<OwnedKernelStack> {
    let mut boxed = Box::<OwnedKernelStack>::new_uninit();
    // SAFETY: OwnedKernelStack is a byte array whose all-zero value is valid.
    unsafe {
        core::ptr::write_bytes(
            boxed.as_mut_ptr().cast::<u8>(),
            0,
            core::mem::size_of::<OwnedKernelStack>(),
        );
        boxed.assume_init()
    }
}

impl Scheduler {
    pub(super) fn transition_new(thread_capacity: usize, rr_quantum_ms: u64) -> Self {
        Self {
            lifecycle: ThreadTable::with_capacity(thread_capacity),
            rr_quantum_ms: rr_quantum_ms.max(1),
            entered_running_thread: false,
        }
    }

    pub(super) fn transition_append_bootstrap(
        &mut self,
        entry: thread::ThreadEntry,
        arg: thread::ThreadArg,
        idle: bool,
    ) -> usize {
        let id = self.lifecycle.slots.len();
        self.lifecycle.slots.push(ThreadSlot::free());
        self.transition_publish_bootstrap(id, entry, arg, idle);
        id
    }

    pub(super) fn transition_fill_free_slots(&mut self, thread_capacity: usize) {
        while self.lifecycle.slots.len() < thread_capacity {
            let id = self.lifecycle.slots.len();
            self.lifecycle.slots.push(ThreadSlot::free());
            self.lifecycle.free_slots.push(id);
            crate::trace!("sched: prepared free thread slot {id}");
        }
    }

    pub(super) fn transition_thread_count(&self) -> usize {
        self.lifecycle.slots.len()
    }

    pub(super) fn transition_ready_capacity(&self) -> usize {
        self.lifecycle.ready_queue.capacity()
    }

    pub(super) fn transition_has_ready(&self) -> bool {
        !self.lifecycle.ready_queue.is_empty()
    }

    pub(super) fn take_free_slot(&mut self) -> Option<usize> {
        self.lifecycle.free_slots.pop()
    }

    pub(super) fn transition_publish_bootstrap(
        &mut self,
        id: usize,
        entry: thread::ThreadEntry,
        arg: thread::ThreadArg,
        idle: bool,
    ) {
        if idle != (id == IDLE_THREAD_INDEX) {
            panic!("sched: bootstrap idle identity mismatch");
        }
        let context = SavedContext::kernel_entry(
            self.lifecycle.slots[id]
                .stack
                .as_ref()
                .unwrap_or_else(|| panic!("sched: bootstrap slot {id} has no stack"))
                .top(),
            entry as *const () as usize,
            arg.as_usize(),
            thread::thread_entry_bootstrap as *const () as usize,
        );
        self.publish(id, context, None, false, INITIAL_THREAD_GENERATION, true);
    }

    pub(super) fn transition_publish_runtime(
        &mut self,
        id: usize,
        context: SavedContext,
        user: Option<UserThreadResources>,
        joinable: bool,
    ) -> ThreadId {
        let generation = next_generation(self.lifecycle.slots[id].generation);
        self.publish(id, context, user, joinable, generation, false);
        self.thread_id(id)
    }

    fn publish(
        &mut self,
        id: usize,
        context: SavedContext,
        user: Option<UserThreadResources>,
        joinable: bool,
        generation: u32,
        bootstrap: bool,
    ) {
        let slot = &mut self.lifecycle.slots[id];
        if slot.thread.is_some() {
            panic!("sched: publish into occupied slot {id}");
        }
        let stack = slot
            .stack
            .take()
            .unwrap_or_else(|| panic!("sched: free slot {id} has no kernel stack"));
        slot.generation = generation;
        slot.thread = Some(Thread {
            state: ThreadState::Ready,
            joinable,
            exit_code: None,
            joiner: None,
            last_join_result: None,
            reaped_user: None,
            user,
            wait: WaitMetadata::with_next_sequence(slot.next_wait_sequence),
            stack,
            context,
        });
        if id != IDLE_THREAD_INDEX {
            let was_empty = self.lifecycle.ready_queue.is_empty();
            self.ready_push_back(self.thread_id(id));
            if !bootstrap && was_empty {
                self.note_ready_peer();
            }
        }
        self.validate_after_transition();
    }

    pub(super) fn transition_initial_dispatch(&mut self) -> super::Result<()> {
        if self.lifecycle.current.is_some() {
            return Err(super::SchedError::InvalidThreadId);
        }
        let next = self
            .ready_pop_front()
            .unwrap_or_else(|| self.thread_id(IDLE_THREAD_INDEX));
        self.make_running(next);
        self.validate_after_transition();
        Ok(())
    }

    pub(super) fn transition_optional_switch(&mut self) -> SwitchOutcome {
        let Some(from) = self.lifecycle.current else {
            return SwitchOutcome::ContinueCurrent;
        };
        self.assert_current_running(from, "optional switch");
        let Some(to) = self.ready_pop_front() else {
            self.validate_after_transition();
            return SwitchOutcome::ContinueCurrent;
        };
        // The checkpoint already consumed the request that selected this
        // switch. Requeueing the outgoing thread must not create a second one.
        self.make_ready_and_queue(from, false);
        self.make_running(to);
        self.validate_after_transition();
        SwitchOutcome::Switch { from, to }
    }

    pub(super) fn transition_commit_wait(
        &mut self,
        prepared: PreparedWait,
    ) -> (CommitState, Option<SwitchOutcome>) {
        crate::sync::preempt::assert_preemption_enabled("scheduler blocking transition");
        let from = self
            .lifecycle
            .current
            .unwrap_or_else(|| panic!("block requested without a running thread"));
        self.assert_current_running(from, "blocking transition");
        if from.index() == IDLE_THREAD_INDEX {
            panic!("sched: idle thread cannot block");
        }
        let commit = self.thread_mut(from.index()).wait.commit(prepared);
        let CommitState::Block(_) = commit else {
            self.validate_after_transition();
            return (commit, None);
        };
        let to = self
            .ready_pop_front()
            .unwrap_or_else(|| self.thread_id(IDLE_THREAD_INDEX));
        self.thread_mut(from.index()).state = ThreadState::Blocked;
        self.make_running(to);
        self.validate_after_transition();
        (commit, Some(SwitchOutcome::Switch { from, to }))
    }

    pub(super) fn transition_prepare_wait(&mut self) -> PreparedWait {
        crate::sync::preempt::assert_preemption_enabled("scheduler wait preparation");
        let current = self
            .lifecycle
            .current
            .unwrap_or_else(|| panic!("sched: wait preparation without a running thread"));
        self.assert_current_running(current, "wait preparation");
        let prepared = self.thread_mut(current.index()).wait.prepare(current);
        self.validate_after_transition();
        prepared
    }

    pub(super) fn transition_complete_wait(
        &mut self,
        token: WaitToken,
        cause: WaitCause,
    ) -> CompletionResult {
        if !self.thread_matches(token.thread()) {
            return CompletionResult::Stale;
        }
        let id = token.thread().index();
        let result = self.thread_mut(id).wait.complete(token, cause);
        if result == CompletionResult::WokeBlocked {
            if self.thread_state(id) != ThreadState::Blocked {
                panic!("sched: blocked wait completion has non-blocked thread state");
            }
            self.make_ready_and_queue(token.thread(), true);
        } else if result == CompletionResult::CompletedPrepared
            && self.thread_state(id) != ThreadState::Running
        {
            panic!("sched: prepared wait completion has non-running thread state");
        }
        self.validate_after_transition();
        result
    }

    pub(super) fn transition_finish_wait(
        &mut self,
        token: WaitToken,
    ) -> Result<WaitCause, FinishError> {
        if !self.thread_matches(token.thread()) || self.lifecycle.current != Some(token.thread()) {
            return Err(FinishError::Stale);
        }
        self.assert_current_running(token.thread(), "wait finish");
        let result = self.thread_mut(token.thread().index()).wait.finish(token);
        self.validate_after_transition();
        result
    }

    pub(super) fn transition_cancel_wait(&mut self, prepared: PreparedWait) -> CancelResult {
        let token = prepared.token();
        if !self.thread_matches(token.thread()) || self.lifecycle.current != Some(token.thread()) {
            return CancelResult::Stale;
        }
        self.assert_current_running(token.thread(), "wait cancellation");
        let result = self
            .thread_mut(token.thread().index())
            .wait
            .cancel(prepared);
        self.validate_after_transition();
        result
    }

    pub(super) fn transition_exit_current(&mut self, code: usize) -> SwitchOutcome {
        let from = self
            .lifecycle
            .current
            .unwrap_or_else(|| panic!("thread: exit without running thread"));
        self.assert_current_running(from, "exit transition");
        if from.index() == IDLE_THREAD_INDEX {
            panic!("thread: idle thread cannot exit");
        }
        let joinable = self.thread(from.index()).joinable;
        let joiner = self.thread_mut(from.index()).joiner.take();
        self.thread_mut(from.index()).exit_code = Some(code);
        if let Some(joiner) = joiner {
            // Publish the lifecycle result and complete the exact join wait
            // while `from` is still the current Running thread. Completion runs
            // the transition validator, so exposing an intermediate
            // current-but-Exited state here would violate the scheduler table
            // invariant before mandatory exit selection is committed.
            self.complete_joiner(from, joiner, code);
        }
        self.thread_mut(from.index()).state = ThreadState::Exited;
        if let Some(joiner) = joiner {
            let resources = self.reap_exited(from);
            self.set_reaped_user(joiner.thread(), resources);
        } else if !joinable {
            let _ = self.reap_exited(from);
        }
        let to = self
            .ready_pop_front()
            .unwrap_or_else(|| self.thread_id(IDLE_THREAD_INDEX));
        self.make_running(to);
        self.validate_after_transition();
        SwitchOutcome::Switch { from, to }
    }

    pub(super) fn transition_reap_exited(&mut self, id: ThreadId) -> Option<UserThreadResources> {
        if id.index() >= self.lifecycle.slots.len()
            || !self.thread_matches(id)
            || id.index() == IDLE_THREAD_INDEX
            || self.lifecycle.current == Some(id)
            || self.thread(id.index()).state != ThreadState::Exited
        {
            panic!("thread: reclaim requires a non-current exited thread");
        }
        if self
            .lifecycle
            .ready_queue
            .iter()
            .any(|queued| queued.index() == id.index())
        {
            panic!("thread: reclaim target still queued ready");
        }
        let resources = self.reap_exited(id);
        self.validate_after_transition();
        resources
    }

    fn reap_exited(&mut self, id: ThreadId) -> Option<UserThreadResources> {
        self.lifecycle.release(id)
    }

    fn complete_joiner(&mut self, target: ThreadId, joiner: WaitToken, code: usize) {
        if self.transition_complete_wait(joiner, WaitCause::Notified)
            != CompletionResult::WokeBlocked
        {
            panic!("thread: joiner {joiner:?} has invalid state while target {target} exited");
        }
        // Exact completion validates generation and wait sequence before the
        // lifecycle owner writes payload into the joiner's reusable slot.
        self.thread_mut(joiner.thread().index()).last_join_result = Some(Ok(code));
    }

    fn ready_push_back(&mut self, id: ThreadId) {
        if id.index() == IDLE_THREAD_INDEX {
            panic!("sched: idle must not enter ready queue");
        }
        if let Some(queued) = self
            .lifecycle
            .ready_queue
            .iter()
            .find(|queued| queued.index() == id.index())
        {
            panic!("sched: duplicate or stale ready queue entry {queued} for {id}");
        }
        if self.lifecycle.ready_queue.len() == self.lifecycle.ready_queue.capacity() {
            panic!("sched: ready queue capacity exhausted");
        }
        self.lifecycle.ready_queue.push_back(id);
    }

    fn ready_pop_front(&mut self) -> Option<ThreadId> {
        let id = self.lifecycle.ready_queue.pop_front()?;
        if !self.thread_matches(id)
            || self.thread(id.index()).state != ThreadState::Ready
            || id.index() == IDLE_THREAD_INDEX
        {
            panic!("sched: stale or invalid ready queue entry {id}");
        }
        Some(id)
    }

    fn make_ready_and_queue(&mut self, id: ThreadId, notify_new_peer: bool) {
        if !self.thread_matches(id) {
            panic!("sched: ready transition requires a current-generation thread {id}");
        }
        self.thread_mut(id.index()).state = ThreadState::Ready;
        if id.index() != IDLE_THREAD_INDEX {
            let was_empty = self.lifecycle.ready_queue.is_empty();
            self.ready_push_back(id);
            if notify_new_peer && was_empty {
                self.note_ready_peer();
            }
        }
    }

    fn assert_current_running(&self, id: ThreadId, operation: &str) {
        if self.lifecycle.current != Some(id)
            || !self.thread_matches(id)
            || self.thread_state(id.index()) != ThreadState::Running
        {
            panic!("sched: {operation} requires the current running thread {id}");
        }
    }

    fn make_running(&mut self, id: ThreadId) {
        if !self.thread_matches(id) || self.thread(id.index()).state != ThreadState::Ready {
            panic!("sched: running transition requires a ready current-generation thread");
        }
        self.thread_mut(id.index()).state = ThreadState::Running;
        self.lifecycle.current = Some(id);
    }

    fn note_ready_peer(&mut self) {
        self.note_runnable_peer_available();
    }

    pub(super) fn current_thread(&self) -> Option<ThreadId> {
        self.lifecycle.current
    }

    pub(super) fn thread_id(&self, id: usize) -> ThreadId {
        ThreadId::new(id, self.lifecycle.slots[id].generation)
    }

    pub(super) fn thread_matches(&self, id: ThreadId) -> bool {
        self.lifecycle.get(id).is_some()
    }

    fn thread(&self, id: usize) -> &Thread {
        self.lifecycle.slots[id]
            .thread
            .as_ref()
            .unwrap_or_else(|| panic!("sched: free slot {id} has no thread"))
    }

    fn thread_mut(&mut self, id: usize) -> &mut Thread {
        self.lifecycle.slots[id]
            .thread
            .as_mut()
            .unwrap_or_else(|| panic!("sched: free slot {id} has no thread"))
    }

    pub(super) fn thread_state(&self, id: usize) -> ThreadState {
        self.thread(id).state
    }

    pub(super) fn thread_address_space(&self, id: usize) -> Option<AddressSpaceId> {
        self.thread(id)
            .user
            .as_ref()
            .map(UserThreadResources::address_space)
    }
    pub(super) fn thread_user_stack(&self, id: usize) -> Option<&OwnedUserStack> {
        self.thread(id)
            .user
            .as_ref()
            .map(UserThreadResources::stack)
    }

    pub(super) fn saved_context(&self, id: ThreadId) -> &SavedContext {
        &self.thread(id.index()).context
    }

    pub(super) fn saved_context_mut(&mut self, id: ThreadId) -> &mut SavedContext {
        &mut self.thread_mut(id.index()).context
    }

    pub(super) fn stack_top(&self, id: usize) -> usize {
        self.lifecycle.slots[id]
            .thread
            .as_ref()
            .map(|thread| thread.stack.top())
            .or_else(|| {
                self.lifecycle.slots[id]
                    .stack
                    .as_ref()
                    .map(|stack| stack.top())
            })
            .unwrap_or_else(|| panic!("sched: slot {id} has no kernel stack"))
    }

    pub(super) fn thread_joinable(&self, id: usize) -> bool {
        self.thread(id).joinable
    }
    pub(super) fn thread_joiner(&self, id: usize) -> Option<WaitToken> {
        self.thread(id).joiner
    }
    pub(super) fn thread_exit_code(&self, id: usize) -> Option<usize> {
        self.thread(id).exit_code
    }
    pub(super) fn thread_set_joiner(&mut self, id: usize, joiner: WaitToken) {
        self.thread_mut(id).joiner = Some(joiner);
    }
    pub(super) fn set_join_result(
        &mut self,
        id: usize,
        result: core::result::Result<usize, thread::JoinError>,
    ) {
        self.thread_mut(id).last_join_result = Some(result);
    }
    pub(super) fn take_join_result(
        &mut self,
        id: usize,
    ) -> Option<core::result::Result<usize, thread::JoinError>> {
        self.thread_mut(id).last_join_result.take()
    }
    pub(super) fn has_join_result(&self, id: usize) -> bool {
        self.thread(id).last_join_result.is_some()
    }
    pub(super) fn take_reaped_user(&mut self, id: usize) -> Option<UserThreadResources> {
        self.thread_mut(id).reaped_user.take()
    }
    pub(super) fn has_reaped_user(&self, id: usize) -> bool {
        self.thread(id).reaped_user.is_some()
    }
    pub(super) fn set_reaped_user(&mut self, id: ThreadId, user: Option<UserThreadResources>) {
        let joiner = self
            .lifecycle
            .get_mut(id)
            .unwrap_or_else(|| panic!("sched: stale joiner while reaping {id}"));
        if joiner.reaped_user.is_some() {
            // An invariant failure must not unwind through `user`: its stack
            // destructor frees frames and is forbidden while this transition
            // still holds local IRQ exclusion. The halted kernel may leak the
            // incoming resource, but it must never release it in this context.
            core::mem::forget(user);
            panic!("sched: joiner {id} retains unreaped user resources");
        }
        joiner.reaped_user = user;
    }
    pub(super) fn replace_current_user_payload(
        &mut self,
        user: UserThreadResources,
    ) -> core::result::Result<UserThreadResources, ()> {
        let current = self.lifecycle.current.ok_or(())?;
        let thread = self.thread_mut(current.index());
        thread.user.replace(user).ok_or(())
    }

    fn validate_after_transition(&self) {
        #[cfg(any(debug_assertions, feature = "qemu-test"))]
        self.validate_invariants();
    }

    #[cfg(any(debug_assertions, feature = "qemu-test"))]
    pub(super) fn validate_invariants(&self) {
        if self.lifecycle.ready_queue.len() > self.lifecycle.ready_queue.capacity() {
            panic!("sched: ready queue exceeds capacity");
        }
        let mut running = 0usize;
        for (index, slot) in self.lifecycle.slots.iter().enumerate() {
            let id = index;
            let tid = ThreadId::new(index, slot.generation);
            let queued = self
                .lifecycle
                .ready_queue
                .iter()
                .filter(|queued| **queued == tid)
                .count();
            if queued > 1 {
                panic!("sched: duplicate ready entry {tid}");
            }
            let Some(thread) = slot.thread.as_ref() else {
                if slot.stack.is_none() || queued != 0 {
                    panic!("sched: free slot {id} retains lifecycle metadata");
                }
                continue;
            };
            if slot.stack.is_some() {
                panic!("sched: occupied thread {id} does not own its kernel stack");
            }
            match thread.state {
                ThreadState::Ready => {
                    if id == IDLE_THREAD_INDEX {
                        if queued != 0 {
                            panic!("sched: idle queued while ready");
                        }
                    } else if queued != 1 {
                        panic!("sched: ready slot {id} queue mismatch");
                    }
                }
                ThreadState::Running => {
                    running += 1;
                    if self.lifecycle.current != Some(tid) || queued != 0 {
                        panic!("sched: running slot {id} identity mismatch");
                    }
                }
                ThreadState::Blocked => {
                    if queued != 0 || !thread.wait.is_blocked() {
                        panic!("sched: blocked slot {id} wait/queue/context mismatch");
                    }
                }
                ThreadState::Exited => {
                    if queued != 0 || !thread.wait.is_none() {
                        panic!("sched: exited thread {id} wait/queue/context mismatch");
                    }
                }
            }
            if thread.wait.is_prepared() && thread.state != ThreadState::Running {
                panic!("sched: prepared wait is not current running thread {id}");
            }
            if thread.wait.is_completed()
                && !matches!(thread.state, ThreadState::Ready | ThreadState::Running)
            {
                panic!("sched: completed wait has invalid thread state {id}");
            }
            if thread.wait.is_blocked() && thread.state != ThreadState::Blocked {
                panic!("sched: blocked wait has invalid thread state {id}");
            }
        }
        for queued in &self.lifecycle.ready_queue {
            if !self.thread_matches(*queued)
                || self.thread_state(queued.index()) != ThreadState::Ready
                || queued.index() == IDLE_THREAD_INDEX
            {
                panic!("sched: stale ready queue entry {queued}");
            }
        }
        if self.lifecycle.current.is_none() {
            if running != 0 {
                panic!("sched: running thread exists before initial dispatch");
            }
        } else if running != 1 {
            panic!("sched: expected exactly one running thread");
        }
    }
}

fn next_generation(generation: u32) -> u32 {
    let next = generation.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publish_joinable(scheduler: &mut Scheduler, slot: usize) -> ThreadId {
        let context = SavedContext::kernel_entry(scheduler.stack_top(slot), 0, 0, 0);
        scheduler.transition_publish_runtime(slot, context, None, true)
    }

    #[test]
    fn thread_table_preallocates_bounded_free_slots() {
        let mut scheduler = Scheduler::transition_new(2, 1);
        scheduler.transition_fill_free_slots(2);
        assert_eq!(scheduler.transition_thread_count(), 2);
        assert_eq!(scheduler.lifecycle.free_slots.len(), 2);
        assert!(
            scheduler
                .lifecycle
                .slots
                .iter()
                .all(|slot| slot.thread.is_none() && slot.stack.is_some())
        );
        assert_eq!(scheduler.take_free_slot(), Some(1));
        assert_eq!(scheduler.take_free_slot(), Some(0));
        assert_eq!(scheduler.take_free_slot(), None);
        assert!(scheduler.lifecycle.ready_queue.capacity() >= 1);
    }

    #[test]
    fn free_slot_is_not_a_valid_thread_and_reuse_changes_generation() {
        let mut scheduler = Scheduler::transition_new(2, 1);
        scheduler.transition_fill_free_slots(2);
        let slot = scheduler.take_free_slot().expect("bounded free slot");
        let stale = ThreadId::new(slot, scheduler.lifecycle.slots[slot].generation);
        assert!(!scheduler.thread_matches(stale));
        assert!(scheduler.lifecycle.get(stale).is_none());
        let first_id = publish_joinable(&mut scheduler, slot);
        assert!(scheduler.thread_matches(first_id));
        assert!(scheduler.lifecycle.get(first_id).is_some());
        assert!(scheduler.lifecycle.get_mut(first_id).is_some());
        assert!(scheduler.lifecycle.slots[slot].stack.is_none());
        scheduler.thread_mut(slot).state = ThreadState::Exited;
        scheduler.lifecycle.ready_queue.clear();
        scheduler.transition_reap_exited(first_id);
        assert!(!scheduler.thread_matches(first_id));
        assert!(scheduler.lifecycle.get(first_id).is_none());
        assert!(scheduler.lifecycle.slots[slot].stack.is_some());
        assert_eq!(scheduler.lifecycle.slots[slot].next_wait_sequence, 0);
        let reused_slot = scheduler.take_free_slot().expect("released slot");
        assert_eq!(reused_slot, slot);
        let reused = publish_joinable(&mut scheduler, reused_slot);
        assert_eq!(reused.index(), first_id.index());
        assert_ne!(reused.generation(), first_id.generation());
    }

    #[test]
    fn wait_sequence_survives_thread_release_and_slot_reuse() {
        let mut scheduler = Scheduler::transition_new(2, 1);
        scheduler.transition_fill_free_slots(2);
        let slot = scheduler.take_free_slot().expect("bounded free slot");
        let first_id = publish_joinable(&mut scheduler, slot);
        scheduler.thread_mut(slot).state = ThreadState::Running;
        let first = scheduler
            .thread_mut(slot)
            .wait
            .prepare(first_id)
            .token()
            .sequence();
        scheduler.thread_mut(slot).wait.clear_active();
        scheduler.thread_mut(slot).state = ThreadState::Exited;
        scheduler.lifecycle.ready_queue.clear();
        scheduler.transition_reap_exited(first_id);

        let reused_slot = scheduler.take_free_slot().expect("released slot");
        let reused_id = publish_joinable(&mut scheduler, reused_slot);
        let second = scheduler
            .thread_mut(reused_slot)
            .wait
            .prepare(reused_id)
            .token()
            .sequence();
        assert_eq!(reused_slot, slot);
        assert!(second > first);
    }
}
