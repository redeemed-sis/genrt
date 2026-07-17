//! Typed, scheduler-owned identities for one blocking episode.
//!
//! External owners retain their condition payloads and store only a [`WaitToken`].
//! The transition layer consumes this module's inline state machine while it owns
//! lifecycle state and ready-queue membership.

use crate::{arch::ActiveContext, sync::LocalIrqGuard, task::ThreadId};

use super::{Scheduler, scheduler_mut, transition::SwitchOutcome};

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
/// store a wait payload in the scheduler. Completion validates both the thread
/// generation and this per-slot sequence under scheduler transition ownership.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct WaitToken {
    thread: ThreadId,
    sequence: WaitSequence,
}

impl WaitToken {
    /// Return the generation-aware thread identity that owns this wait.
    ///
    /// # Returns
    ///
    /// Returns the token's copyable thread identity without changing scheduler
    /// state, allocating, blocking, or touching IRQ state.
    pub(crate) const fn thread(self) -> ThreadId {
        self.thread
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

/// Coarse diagnostic classification of an external wait owner.
///
/// This type is not a payload channel. In particular, it never contains a
/// mailbox message, process status, target identity, or device-specific state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum WaitKind {
    /// Time owns an absolute deadline registration.
    Deadline,
    /// A bounded mailbox owns condition availability and its waiter queue.
    Ipc,
    /// Thread lifecycle owns a join relationship and exit result.
    Thread,
    /// Process lifecycle owns a terminal status and parent relationship.
    Process,
    /// Console or another I/O owner owns data availability.
    Io,
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
    /// The token completed while its task was still running and prepared.
    CompletedPrepared,
    /// The token completed a blocked task and made it ready exactly once.
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
    /// The current task was made blocked and the supplied handoff was applied.
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

/// Inline scheduler metadata for one occupied slot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct WaitMetadata {
    next_sequence: u64,
    state: WaitState,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum WaitState {
    None,
    Prepared {
        token: WaitToken,
        kind: WaitKind,
    },
    Blocked {
        token: WaitToken,
        kind: WaitKind,
    },
    Completed {
        token: WaitToken,
        cause: WaitCause,
        kind: WaitKind,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum CommitState {
    Block(WaitToken),
    Early(WaitCause),
    Stale,
}

impl WaitMetadata {
    pub(super) const fn empty() -> Self {
        Self {
            next_sequence: 0,
            state: WaitState::None,
        }
    }

    pub(super) fn prepare(&mut self, thread: ThreadId, kind: WaitKind) -> PreparedWait {
        if self.state != WaitState::None {
            panic!("sched: new wait before prior completion is consumed");
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .unwrap_or_else(|| panic!("sched: wait sequence overflow before publication"));
        let token = WaitToken {
            thread,
            sequence: WaitSequence(self.next_sequence),
        };
        self.state = WaitState::Prepared { token, kind };
        PreparedWait { token }
    }

    pub(super) fn commit(&mut self, prepared: PreparedWait) -> CommitState {
        match self.state {
            WaitState::Prepared { token, kind } if token == prepared.token => {
                self.state = WaitState::Blocked { token, kind };
                CommitState::Block(token)
            }
            WaitState::Completed { token, cause, .. } if token == prepared.token => {
                self.state = WaitState::None;
                CommitState::Early(cause)
            }
            _ => CommitState::Stale,
        }
    }

    pub(super) fn complete(&mut self, token: WaitToken, cause: WaitCause) -> CompletionResult {
        match self.state {
            WaitState::Prepared {
                token: active,
                kind,
            } if active == token => {
                self.state = WaitState::Completed { token, cause, kind };
                CompletionResult::CompletedPrepared
            }
            WaitState::Blocked {
                token: active,
                kind,
            } if active == token => {
                self.state = WaitState::Completed { token, cause, kind };
                CompletionResult::WokeBlocked
            }
            WaitState::Completed { token: active, .. } if active == token => {
                CompletionResult::AlreadyCompleted
            }
            _ => CompletionResult::Stale,
        }
    }

    pub(super) fn cancel(&mut self, prepared: PreparedWait) -> CancelResult {
        match self.state {
            WaitState::Prepared { token, .. } if token == prepared.token => {
                self.state = WaitState::None;
                CancelResult::Cancelled
            }
            WaitState::Completed { token, cause, .. } if token == prepared.token => {
                self.state = WaitState::None;
                CancelResult::Completed(cause)
            }
            _ => CancelResult::Stale,
        }
    }

    pub(super) fn finish(&mut self, token: WaitToken) -> Result<WaitCause, FinishError> {
        match self.state {
            WaitState::Completed {
                token: active,
                cause,
                ..
            } if active == token => {
                self.state = WaitState::None;
                Ok(cause)
            }
            WaitState::Prepared { token: active, .. }
            | WaitState::Blocked { token: active, .. }
                if active == token =>
            {
                Err(FinishError::NotCompleted)
            }
            _ => Err(FinishError::Stale),
        }
    }

    pub(super) fn clear_active(&mut self) {
        self.state = WaitState::None;
    }

    pub(super) const fn is_none(&self) -> bool {
        matches!(self.state, WaitState::None)
    }

    pub(super) const fn is_prepared(&self) -> bool {
        matches!(self.state, WaitState::Prepared { .. })
    }

    pub(super) const fn is_blocked(&self) -> bool {
        matches!(self.state, WaitState::Blocked { .. })
    }

    pub(super) const fn is_completed(&self) -> bool {
        matches!(self.state, WaitState::Completed { .. })
    }

    #[cfg(test)]
    fn set_next_sequence_for_test(&mut self, sequence: u64) {
        self.next_sequence = sequence;
    }
}

/// Prepare the current running task for an externally owned wait condition.
///
/// Callers publish [`PreparedWait::token`] while holding their owner lock, then
/// drop that lock before calling [`commit_wait`]. The controlled task-call
/// entry already masks IRQs; this function allocates nothing and does not
/// block.
///
/// # Arguments
///
/// * `kind` - Coarse diagnostic owner classification with no condition payload.
///
/// # Returns
///
/// Returns a non-copyable preparation whose exact token is safe to copy into a
/// bounded external waiter queue.
///
/// # Panics
///
/// Panics when no current task is running, a prior wait is unfinished, or the
/// per-slot sequence would overflow before publication.
pub(crate) fn prepare_wait(kind: WaitKind) -> PreparedWait {
    scheduler_mut().transition_prepare_wait(kind)
}

/// Commit a published wait registration and apply its mandatory handoff.
///
/// The external owner lock must already be dropped. This function either saves
/// and replaces the live context for a bounded mandatory switch or consumes an
/// early completion without switching. It allocates nothing.
///
/// # Arguments
///
/// * `context` - Exclusive task-call context saved if the prepared task blocks.
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
    scheduler_mut().commit_prepared_wait(context, prepared)
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

/// Consume the completion retained for the current running task's exact token.
///
/// This is called by task-call wrappers after their saved context resumes. It
/// takes local IRQ exclusion, allocates nothing, and cannot consume another
/// wait's result.
///
/// # Arguments
///
/// * `token` - Exact token published by the operation before its task call.
///
/// # Returns
///
/// Returns the winning completion cause after clearing inline wait metadata.
///
/// # Errors
///
/// Returns [`FinishError::Stale`] for another generation/sequence/current task,
/// or [`FinishError::NotCompleted`] when called before completion.
pub(crate) fn finish_wait(token: WaitToken) -> Result<WaitCause, FinishError> {
    let _irq_guard = LocalIrqGuard::save_and_disable();
    scheduler_mut().transition_finish_wait(token)
}

/// Cancel a still-prepared current wait without directly waking a blocked task.
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

impl Scheduler {
    pub(super) fn commit_prepared_wait(
        &mut self,
        context: &mut ActiveContext<'_>,
        prepared: PreparedWait,
    ) -> CommitResult {
        let (state, outcome) = self.transition_commit_wait(prepared);
        match (state, outcome) {
            (CommitState::Early(cause), None) => CommitResult::Early(cause),
            (CommitState::Stale, None) => CommitResult::Stale,
            (CommitState::Block(_), Some(switch_outcome @ SwitchOutcome::Switch { from, to })) => {
                self.saved_context_mut(from).save_from(context);
                self.saved_context(to).restore_into(context);
                self.activate_task_address_space(to);
                self.finish_block_current(from, to);
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
/// * `context` - Exclusive private task-call context for a possible handoff.
/// * `mode` - Fixture-selected bounded completion ordering.
/// * `output` - Stack-owned task-call output that preserves the exact token.
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
    output: &mut crate::task_call::TaskCallWaitOutput,
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

    let prepared = prepare_wait(WaitKind::Io);
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
            let next = prepare_wait(WaitKind::Io);
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

    fn prepared() -> (WaitMetadata, PreparedWait) {
        let mut wait = WaitMetadata::empty();
        let prepared = wait.prepare(THREAD, WaitKind::Ipc);
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
    fn cancel_and_first_completion_win() {
        let (mut wait, prepared) = prepared();
        assert_eq!(wait.cancel(prepared), CancelResult::Cancelled);
        let prepared = wait.prepare(THREAD, WaitKind::Ipc);
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
        let prepared = wait.prepare(THREAD, WaitKind::Ipc);
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
            let _ = wait.prepare(THREAD, WaitKind::Ipc);
        }));
        assert!(rejected.is_err());
        assert_eq!(wait.finish(token), Ok(WaitCause::Notified));
        let _next = wait.prepare(THREAD, WaitKind::Ipc);
    }

    #[test]
    fn stale_sequence_generation_and_unfinished_rejection() {
        let (mut wait, prepared) = prepared();
        let token = prepared.token();
        let stale_sequence = WaitToken {
            thread: THREAD,
            sequence: WaitSequence(99),
        };
        let stale_generation = WaitToken {
            thread: ThreadId::new(1, 8),
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
        assert_eq!(wait.commit(prepared), CommitState::Block(token));
        assert_eq!(wait.finish(token), Err(FinishError::NotCompleted));
    }

    #[test]
    #[should_panic(expected = "wait sequence overflow")]
    fn sequence_overflow_panics_before_publication() {
        let mut wait = WaitMetadata::empty();
        wait.set_next_sequence_for_test(u64::MAX);
        let _ = wait.prepare(THREAD, WaitKind::Deadline);
    }
}
