//! Typed, scheduler-owned identities for one blocking episode.
//!
//! External owners retain their condition payloads and store only a [`WaitToken`].
//! The transition layer consumes this module's inline state machine while it owns
//! lifecycle state and ready-queue membership.

use crate::{arch::ActiveContext, cpu::CpuId, sched::ThreadId, sync::LocalIrqGuard};

use super::{scheduler_mut, transition::SwitchOutcome};

/// Monotonic per-slot identity for one wait registration.
///
/// A sequence is never reset when a scheduler slot is reaped. It is paired with
/// a generation-checked [`ThreadId`] in [`WaitToken`], so a delayed completion
/// cannot name either a later wait or a reused slot.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct WaitSequence(u64);

/// Copyable identity for one exact scheduler wait registration.
///
/// External bounded queues may retain this token, but they must not infer or
/// store a wait payload in the scheduler. Completion validates the thread
/// generation and per-slot sequence under scheduler transition ownership. The
/// captured immutable home CPU identifies the deadline queue that may time out
/// this wait.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct WaitToken {
    thread: ThreadId,
    cpu: CpuId,
    sequence: WaitSequence,
}

impl WaitToken {
    /// Construct an isolated host-test identity without publishing a wait.
    /// # Arguments
    /// * `thread` - Generation-bearing test thread.
    /// * `cpu` - Immutable test CPU.
    /// * `sequence` - Exact test episode sequence.
    /// # Returns
    /// Copyable token without allocation, scheduler or IRQ state changes.
    #[cfg(test)]
    pub(crate) const fn for_profile_test(thread: ThreadId, cpu: CpuId, sequence: u64) -> Self {
        Self {
            thread,
            cpu,
            sequence: WaitSequence(sequence),
        }
    }

    /// Return the generation-aware thread identity that owns this wait.
    ///
    /// # Returns
    ///
    /// Returns the token's copyable thread identity without changing scheduler
    /// state, allocating, blocking, or touching IRQ state.
    pub(crate) const fn thread(self) -> ThreadId {
        self.thread
    }

    /// Return the immutable home CPU that owns this wait's timed events.
    ///
    /// # Returns
    ///
    /// Returns the logical CPU captured when the current thread prepared this
    /// wait. This copy-only operation does not allocate, block, or alter IRQ
    /// or scheduler state.
    pub(crate) const fn cpu(self) -> CpuId {
        self.cpu
    }

    /// Return the monotonically increasing identity of this wait episode.
    ///
    /// # Returns
    ///
    /// Returns the sequence value without changing scheduler state,
    /// allocating, blocking, or touching IRQ state.
    pub(crate) const fn sequence(self) -> u64 {
        self.sequence.0
    }
}

/// The first external completion cause retained for a wait token.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum WaitCause {
    /// The external owner made the waited condition available.
    Notified,
    /// A time-owned deadline completed the wait first.
    Timeout,
}

/// A single-use prepared registration that has not necessarily blocked yet.
///
/// The value intentionally is neither `Copy` nor `Clone`. Owners may copy its
/// [`WaitToken`] into bounded queues through [`PreparedWait::token`], but only
/// one commit or cancel operation may consume the preparation.
#[must_use = "a prepared wait must be committed or cancelled"]
pub(crate) struct PreparedWait {
    token: WaitToken,
    consumption: WaitConsumption,
}

impl PreparedWait {
    /// Return the copyable exact token to publish with the external owner.
    ///
    /// # Returns
    ///
    /// Returns the token without consuming the preparation, allocating,
    /// blocking, or changing IRQ state.
    pub(crate) const fn token(&self) -> WaitToken {
        self.token
    }
}

/// Controlled result of an external completion attempt.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum CompletionResult {
    /// The token completed while its thread was still running and prepared.
    CompletedPrepared,
    /// The token completed a blocked thread and made it ready exactly once.
    WokeBlocked,
    /// The exact token had already completed; its first cause remains intact.
    AlreadyCompleted,
    /// The thread generation, sequence, or active token did not match.
    Stale,
}

/// Result of committing one prepared wait through the scheduler handoff.
///
/// `Blocked` carries the mandatory context-switch outcome selected by the
/// transition layer. `Early` proves an external completion won after
/// publication but before blocking, so no handoff occurred.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum CommitResult {
    /// The current thread was made blocked and the supplied handoff was applied.
    Blocked(SwitchOutcome),
    /// A published completion won before blocking and was consumed.
    Early(WaitCause),
    /// The single-use preparation did not match the current scheduler wait.
    Stale,
}

/// Result of consuming a prepared wait without blocking it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum CancelResult {
    /// A still-prepared registration was removed.
    Cancelled,
    /// A completion won before cancellation and its cause was consumed.
    Completed(WaitCause),
    /// The prepared token no longer names the active registration.
    Stale,
}

/// Reason exact completion consumption cannot finish a wait.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FinishError {
    /// The token is stale or belongs to another current wait.
    Stale,
    /// The current matching wait has not completed yet.
    NotCompleted,
}

/// Active wait metadata owned by one occupied thread.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct WaitMetadata {
    next_sequence: u64,
    active: Option<ActiveWait>,
}

// Keep the exact identity stationary while only its lifecycle phase changes.
// Separate enum payloads for Prepared/Blocked force token copies on commit.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ActiveWait {
    token: WaitToken,
    phase: WaitPhase,
    consumption: WaitConsumption,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum WaitPhase {
    Prepared,
    Blocked,
    Completed(WaitCause),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum WaitConsumption {
    Explicit,
    Automatic,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum CommitState {
    Block(WaitToken),
    Early(WaitCause),
    Stale,
}

impl WaitMetadata {
    pub(super) const fn with_next_sequence(next_sequence: u64) -> Self {
        Self {
            next_sequence,
            active: None,
        }
    }

    pub(super) const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub(super) fn prepare(&mut self, thread: ThreadId, cpu: CpuId) -> PreparedWait {
        self.prepare_with_consumption(thread, cpu, WaitConsumption::Explicit)
    }

    pub(super) fn prepare_automatic(&mut self, thread: ThreadId, cpu: CpuId) -> PreparedWait {
        self.prepare_with_consumption(thread, cpu, WaitConsumption::Automatic)
    }

    fn prepare_with_consumption(
        &mut self,
        thread: ThreadId,
        cpu: CpuId,
        consumption: WaitConsumption,
    ) -> PreparedWait {
        if self.active.is_some() {
            panic!("sched: new wait before prior completion is consumed");
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .unwrap_or_else(|| panic!("sched: wait sequence overflow before publication"));
        let token = WaitToken {
            thread,
            cpu,
            sequence: WaitSequence(self.next_sequence),
        };
        self.active = Some(ActiveWait {
            token,
            phase: WaitPhase::Prepared,
            consumption,
        });
        PreparedWait { token, consumption }
    }

    pub(super) fn commit(&mut self, prepared: PreparedWait) -> CommitState {
        let Some(active) = self.active.as_mut() else {
            return CommitState::Stale;
        };
        if active.token != prepared.token {
            return CommitState::Stale;
        }
        match active.phase {
            WaitPhase::Prepared if active.consumption == prepared.consumption => {
                active.phase = WaitPhase::Blocked;
                CommitState::Block(active.token)
            }
            WaitPhase::Completed(cause) => {
                self.active = None;
                CommitState::Early(cause)
            }
            _ => CommitState::Stale,
        }
    }

    pub(super) fn complete(&mut self, token: WaitToken, cause: WaitCause) -> CompletionResult {
        let Some(active) = self.active.as_mut() else {
            return CompletionResult::Stale;
        };
        if active.token != token {
            return CompletionResult::Stale;
        }
        match active.phase {
            WaitPhase::Prepared => {
                active.phase = WaitPhase::Completed(cause);
                CompletionResult::CompletedPrepared
            }
            WaitPhase::Blocked => {
                match active.consumption {
                    WaitConsumption::Explicit => active.phase = WaitPhase::Completed(cause),
                    WaitConsumption::Automatic => self.active = None,
                }
                CompletionResult::WokeBlocked
            }
            WaitPhase::Completed(_) => CompletionResult::AlreadyCompleted,
        }
    }

    pub(super) fn cancel(&mut self, prepared: PreparedWait) -> CancelResult {
        let Some(active) = self.active.as_ref() else {
            return CancelResult::Stale;
        };
        if active.token != prepared.token {
            return CancelResult::Stale;
        }
        match active.phase {
            WaitPhase::Prepared => {
                self.active = None;
                CancelResult::Cancelled
            }
            WaitPhase::Completed(cause) => {
                self.active = None;
                CancelResult::Completed(cause)
            }
            WaitPhase::Blocked => CancelResult::Stale,
        }
    }

    pub(super) fn finish(&mut self, token: WaitToken) -> Result<WaitCause, FinishError> {
        let Some(active) = self.active.as_ref() else {
            return Err(FinishError::Stale);
        };
        if active.token != token {
            return Err(FinishError::Stale);
        }
        match active.phase {
            WaitPhase::Completed(cause) => {
                self.active = None;
                Ok(cause)
            }
            WaitPhase::Prepared | WaitPhase::Blocked => Err(FinishError::NotCompleted),
        }
    }

    #[cfg(test)]
    pub(super) fn clear_active(&mut self) {
        self.active = None;
    }

    pub(super) const fn is_none(&self) -> bool {
        self.active.is_none()
    }

    #[cfg(any(test, debug_assertions, feature = "qemu-test"))]
    pub(super) const fn is_prepared(&self) -> bool {
        matches!(
            self.active,
            Some(ActiveWait {
                phase: WaitPhase::Prepared,
                ..
            })
        )
    }

    #[cfg(any(test, debug_assertions, feature = "qemu-test"))]
    pub(super) const fn is_blocked(&self) -> bool {
        matches!(
            self.active,
            Some(ActiveWait {
                phase: WaitPhase::Blocked,
                ..
            })
        )
    }

    #[cfg(any(test, debug_assertions, feature = "qemu-test"))]
    pub(super) const fn is_completed(&self) -> bool {
        matches!(
            self.active,
            Some(ActiveWait {
                phase: WaitPhase::Completed(_),
                ..
            })
        )
    }

    #[cfg(test)]
    fn set_next_sequence_for_test(&mut self, sequence: u64) {
        self.next_sequence = sequence;
    }
}

/// Prepare the current running thread for an externally owned wait condition.
///
/// Callers publish [`PreparedWait::token`] while holding their owner lock, then
/// drop that lock before calling [`commit_wait`]. The controlled sched-call
/// entry already masks IRQs; this function allocates nothing and does not
/// block.
///
/// # Returns
///
/// Returns a non-copyable preparation whose exact token is safe to copy into a
/// bounded external waiter queue.
///
/// # Panics
///
/// Panics when no current thread is running, a prior wait is unfinished, or the
/// per-slot sequence would overflow before publication.
pub(crate) fn prepare_wait() -> PreparedWait {
    let cpu = super::current_cpu();
    scheduler_mut().on_cpu(cpu).transition_prepare_wait()
}

/// Commit a published wait registration and apply its mandatory handoff.
///
/// The external owner lock must already be dropped. This function either saves
/// and replaces the live context for a bounded mandatory switch or consumes an
/// early completion without switching. It allocates nothing.
///
/// # Arguments
///
/// * `context` - Exclusive sched-call context saved if the prepared thread blocks.
/// * `prepared` - Single-use registration returned by [`prepare_wait`].
///
/// # Returns
///
/// Returns `Blocked` after applying its mandatory handoff, `Early` when a
/// publication-time completion won, or `Stale` for a controlled mismatched
/// registration.
///
/// # Panics
///
/// Panics when blocking is attempted under a preemption guard or a required
/// scheduler handoff invariant is absent.
pub(crate) fn commit_wait(context: &mut ActiveContext<'_>, prepared: PreparedWait) -> CommitResult {
    let cpu = super::current_cpu();
    scheduler_mut()
        .on_cpu(cpu)
        .commit_prepared_wait(context, prepared)
}

/// Attempt first-wins completion of one exact external wait token.
///
/// The owner must claim the token and release its owner lock before this call.
/// The function takes short local IRQ exclusion itself, allocates nothing, and
/// never acquires an external owner lock.
///
/// # Arguments
///
/// * `token` - Exact published wait registration to complete.
/// * `cause` - First-wins external event or deadline cause.
///
/// # Returns
///
/// Returns a controlled completion result; stale and duplicate notifications
/// leave lifecycle and ready-queue state unchanged.
pub(crate) fn complete_wait(token: WaitToken, cause: WaitCause) -> CompletionResult {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    scheduler_mut().transition_complete_wait(token, cause)
}

/// Consume the completion retained for the current running thread's exact token.
///
/// This is called by sched-call wrappers after their saved context resumes. It
/// takes local IRQ exclusion, allocates nothing, and cannot consume another
/// wait's result.
///
/// # Arguments
///
/// * `token` - Exact token published by the operation before its sched call.
///
/// # Returns
///
/// Returns the winning completion cause after clearing inline wait metadata.
///
/// # Errors
///
/// Returns [`FinishError::Stale`] for another generation/sequence/current thread,
/// or [`FinishError::NotCompleted`] when called before completion.
pub(crate) fn finish_wait(token: WaitToken) -> Result<WaitCause, FinishError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    scheduler_mut().transition_finish_wait(token)
}

/// Cancel a still-prepared current wait without directly waking a blocked thread.
///
/// # Arguments
///
/// * `prepared` - Single-use preparation that has not been committed.
///
/// # Returns
///
/// Returns a controlled cancellation result. A racing completion returns its
/// first-wins cause; blocked waits are intentionally stale here and must use
/// [`complete_wait`].
pub(crate) fn cancel_wait(prepared: PreparedWait) -> CancelResult {
    scheduler_mut().transition_cancel_wait(prepared)
}

impl super::CpuScheduler<'_> {
    pub(super) fn commit_prepared_wait(
        &mut self,
        context: &mut ActiveContext<'_>,
        prepared: PreparedWait,
    ) -> CommitResult {
        self.commit_wait_with(context, prepared, Self::finish_block_current)
    }

    /// Commit an EL0 sleep after time publication removed its old quantum.
    /// # Arguments
    /// * `context` - Exclusive live exception return context.
    /// * `prepared` - Exact automatic wait published by the sleep owner.
    /// # Returns
    /// Returns Blocked after handoff, Early after consuming a winning cause,
    /// or Stale for an invalid token. Early callers restore quantum eligibility.
    /// Bounded and allocation-free under the existing scheduler guard.
    /// # Panics
    /// Panics for malformed transitions or architecture handoff failure.
    pub(super) fn commit_prepared_sleep(
        &mut self,
        context: &mut ActiveContext<'_>,
        prepared: PreparedWait,
    ) -> CommitResult {
        self.commit_wait_with(context, prepared, Self::finish_block_after_quantum_cancel)
    }

    fn commit_wait_with(
        &mut self,
        context: &mut ActiveContext<'_>,
        prepared: PreparedWait,
        finish: impl FnOnce(&mut Self, ThreadId, ThreadId),
    ) -> CommitResult {
        let (state, outcome) = self.transition_commit_wait(prepared);
        match (state, outcome) {
            (CommitState::Early(cause), None) => CommitResult::Early(cause),
            (CommitState::Stale, None) => CommitResult::Stale,
            (CommitState::Block(_), Some(switch_outcome @ SwitchOutcome::Switch { from, to })) => {
                #[cfg(feature = "wake-profile")]
                crate::profile::sleep_committed(from);
                self.saved_context_mut(from).save_from(context);
                self.saved_context(to).restore_into(context);
                self.activate_thread_address_space(to);
                #[cfg(feature = "wake-profile")]
                crate::profile::context_restored(to);
                finish(self, from, to);
                #[cfg(feature = "wake-profile")]
                crate::profile::sleep_handoff(from);
                CommitResult::Blocked(switch_outcome)
            }
            _ => panic!("sched: malformed wait commit transition outcome"),
        }
    }
}

/// Run a bounded exact-token scenario for the QEMU kernel contract fixture.
///
/// The fixture uses this isolated seam to exercise publication-time completion,
/// duplicate first-wins behavior, and late-token rejection without exporting a
/// slot-only wake API to production owners.
///
/// # Arguments
///
/// * `context` - Exclusive private sched-call context for a possible handoff.
/// * `mode` - Fixture-selected bounded completion ordering.
/// * `output` - Stack-owned sched-call output that preserves the exact token.
///
/// # Returns
///
/// Returns after a bounded early completion or mandatory handoff. It does not
/// allocate and is compiled only for the QEMU kernel-runtime fixture.
///
/// # Panics
///
/// Panics for an unknown mode or a controlled completion mismatch.
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) fn on_test_wait_sync(
    context: &mut ActiveContext<'_>,
    mode: u64,
    output: &mut crate::sched::call::SchedCallWaitOutput,
) {
    fn complete_prepared(token: WaitToken, first: WaitCause, second: Option<WaitCause>) {
        if complete_wait(token, first) != CompletionResult::CompletedPrepared {
            panic!("sched test: first completion did not complete prepared token");
        }
        if let Some(cause) = second {
            if complete_wait(token, cause) != CompletionResult::AlreadyCompleted {
                panic!("sched test: duplicate completion changed token");
            }
        }
    }

    let prepared = prepare_wait();
    let token = prepared.token();
    output.record_token(token);
    match mode {
        0 => complete_prepared(token, WaitCause::Notified, None),
        1 => complete_prepared(token, WaitCause::Notified, Some(WaitCause::Notified)),
        2 => complete_prepared(token, WaitCause::Notified, Some(WaitCause::Timeout)),
        3 => complete_prepared(token, WaitCause::Timeout, Some(WaitCause::Notified)),
        4 => {
            complete_prepared(token, WaitCause::Notified, None);
            let cause = match commit_wait(context, prepared) {
                CommitResult::Early(cause) => cause,
                _ => panic!("sched test: first token did not complete early"),
            };
            if cause != WaitCause::Notified {
                panic!("sched test: wrong first cause");
            }
            let next = prepare_wait();
            let next_token = next.token();
            output.record_token(next_token);
            if complete_wait(token, WaitCause::Timeout) != CompletionResult::Stale {
                panic!("sched test: late token affected next wait");
            }
            complete_prepared(next_token, WaitCause::Notified, None);
            match commit_wait(context, next) {
                CommitResult::Early(cause) => output.record_early(cause),
                _ => panic!("sched test: next token did not complete early"),
            }
            return;
        }
        5 => {
            crate::test_support::kernel_runtime::publish_wait_token(token);
            match commit_wait(context, prepared) {
                CommitResult::Blocked(_) => {}
                CommitResult::Early(cause) => output.record_early(cause),
                CommitResult::Stale => panic!("sched test: blocking token became stale"),
            }
            return;
        }
        _ => panic!("sched test: unknown wait mode {mode}"),
    }
    match commit_wait(context, prepared) {
        CommitResult::Early(cause) => output.record_early(cause),
        _ => panic!("sched test: completion unexpectedly blocked"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREAD: ThreadId = ThreadId::new(1, 7);

    fn cpu0() -> CpuId {
        CpuId::from_index(0).unwrap()
    }

    fn prepared() -> (WaitMetadata, PreparedWait) {
        let mut wait = WaitMetadata::with_next_sequence(0);
        let prepared = wait.prepare(THREAD, cpu0());
        (wait, prepared)
    }

    #[test]
    fn normal_lifecycle() {
        let (mut wait, prepared) = prepared();
        let token = prepared.token();
        assert_eq!(wait.commit(prepared), CommitState::Block(token));
        assert_eq!(
            wait.complete(token, WaitCause::Notified),
            CompletionResult::WokeBlocked
        );
        assert_eq!(wait.finish(token), Ok(WaitCause::Notified));
        assert!(wait.is_none());
    }

    #[test]
    fn phase_changes_preserve_active_identity_and_first_cause() {
        let (mut wait, prepared) = prepared();
        let token = prepared.token();
        assert_eq!(wait.active.as_ref().unwrap().token, token);
        assert!(wait.is_prepared());
        assert_eq!(wait.commit(prepared), CommitState::Block(token));
        assert_eq!(wait.active.as_ref().unwrap().token, token);
        assert!(wait.is_blocked());
        assert_eq!(
            wait.complete(token, WaitCause::Timeout),
            CompletionResult::WokeBlocked
        );
        assert_eq!(wait.active.as_ref().unwrap().token, token);
        assert!(wait.is_completed());
        assert_eq!(
            wait.complete(token, WaitCause::Notified),
            CompletionResult::AlreadyCompleted
        );
        assert_eq!(wait.finish(token), Ok(WaitCause::Timeout));
        assert!(wait.is_none());
    }

    #[test]
    fn mismatched_preparation_does_not_change_active_wait() {
        let (mut wait, prepared) = prepared();
        let before = wait;
        let wrong_mode = PreparedWait {
            token: prepared.token(),
            consumption: WaitConsumption::Automatic,
        };
        assert_eq!(wait.commit(wrong_mode), CommitState::Stale);
        assert_eq!(wait, before);
        let wrong_token = PreparedWait {
            token: WaitToken {
                sequence: WaitSequence(99),
                ..prepared.token()
            },
            consumption: WaitConsumption::Explicit,
        };
        assert_eq!(wait.cancel(wrong_token), CancelResult::Stale);
        assert_eq!(wait, before);
        assert_eq!(
            wait.commit(prepared),
            CommitState::Block(before.active.unwrap().token)
        );
    }

    #[test]
    fn early_completion_cancels_block() {
        let (mut wait, prepared) = prepared();
        let token = prepared.token();
        assert_eq!(
            wait.complete(token, WaitCause::Timeout),
            CompletionResult::CompletedPrepared
        );
        assert_eq!(
            wait.commit(prepared),
            CommitState::Early(WaitCause::Timeout)
        );
        assert!(wait.is_none());
    }

    #[test]
    fn automatic_wait_clears_on_blocked_completion() {
        let mut wait = WaitMetadata::with_next_sequence(0);
        let prepared = wait.prepare_automatic(THREAD, cpu0());
        let token = prepared.token();
        assert_eq!(wait.commit(prepared), CommitState::Block(token));
        assert_eq!(
            wait.complete(token, WaitCause::Timeout),
            CompletionResult::WokeBlocked
        );
        assert!(wait.is_none());
        assert_eq!(
            wait.complete(token, WaitCause::Timeout),
            CompletionResult::Stale
        );
        let _next = wait.prepare(THREAD, cpu0());
    }

    #[test]
    fn cancel_and_first_completion_win() {
        let (mut wait, prepared) = prepared();
        assert_eq!(wait.cancel(prepared), CancelResult::Cancelled);
        let prepared = wait.prepare(THREAD, cpu0());
        let token = prepared.token();
        assert_eq!(
            wait.complete(token, WaitCause::Notified),
            CompletionResult::CompletedPrepared
        );
        assert_eq!(
            wait.complete(token, WaitCause::Timeout),
            CompletionResult::AlreadyCompleted
        );
        assert_eq!(
            wait.commit(prepared),
            CommitState::Early(WaitCause::Notified)
        );
        let prepared = wait.prepare(THREAD, cpu0());
        let token = prepared.token();
        assert_eq!(
            wait.complete(token, WaitCause::Timeout),
            CompletionResult::CompletedPrepared
        );
        assert_eq!(
            wait.complete(token, WaitCause::Notified),
            CompletionResult::AlreadyCompleted
        );
        assert_eq!(
            wait.commit(prepared),
            CommitState::Early(WaitCause::Timeout)
        );
    }

    #[test]
    fn duplicate_completion_and_new_wait_rejection() {
        let (mut wait, prepared) = prepared();
        let token = prepared.token();
        assert_eq!(wait.commit(prepared), CommitState::Block(token));
        assert_eq!(
            wait.complete(token, WaitCause::Notified),
            CompletionResult::WokeBlocked
        );
        assert_eq!(
            wait.complete(token, WaitCause::Notified),
            CompletionResult::AlreadyCompleted
        );
        let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = wait.prepare(THREAD, cpu0());
        }));
        assert!(rejected.is_err());
        assert_eq!(wait.finish(token), Ok(WaitCause::Notified));
        let _next = wait.prepare(THREAD, cpu0());
    }

    #[test]
    fn stale_sequence_generation_and_unfinished_rejection() {
        let (mut wait, prepared) = prepared();
        let token = prepared.token();
        let stale_sequence = WaitToken {
            thread: THREAD,
            cpu: token.cpu,
            sequence: WaitSequence(99),
        };
        let stale_generation = WaitToken {
            thread: ThreadId::new(1, 8),
            cpu: token.cpu,
            sequence: token.sequence,
        };
        let stale_cpu = WaitToken {
            thread: THREAD,
            cpu: CpuId::from_index(1).unwrap(),
            sequence: token.sequence,
        };
        assert_eq!(
            wait.complete(stale_sequence, WaitCause::Notified),
            CompletionResult::Stale
        );
        assert_eq!(
            wait.complete(stale_generation, WaitCause::Notified),
            CompletionResult::Stale
        );
        assert_eq!(
            wait.complete(stale_cpu, WaitCause::Notified),
            CompletionResult::Stale
        );
        assert_eq!(wait.commit(prepared), CommitState::Block(token));
        assert_eq!(wait.finish(token), Err(FinishError::NotCompleted));
    }

    #[test]
    #[should_panic(expected = "wait sequence overflow")]
    fn sequence_overflow_panics_before_publication() {
        let mut wait = WaitMetadata::with_next_sequence(0);
        wait.set_next_sequence_for_test(u64::MAX);
        let _ = wait.prepare(THREAD, cpu0());
    }
}
