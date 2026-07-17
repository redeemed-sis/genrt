# ADR-0032: Typed wait registrations and external wake ownership

## Status

Accepted

## Context

ADR-0031 centralized scheduler lifecycle transitions, but blocking identity was
still encoded as owner-specific `BlockReason` values. Sleep, mailbox, thread
join, process wait, and stdin each exposed a specialized scheduler block/wake
entry point. A wake identified a thread generation, but not one particular wait
performed by that live thread, so owner queues and deadlines could not reject a
late completion from an earlier operation by identity alone.

The scheduler must protect the lifecycle edge without taking ownership of the
condition or its payload. Mailbox messages, thread exit codes, process status,
UART bytes, and deadline registration all have existing owners and different
cleanup rules.

## Decision

- Every blocking episode has a copyable `WaitToken` containing a
  generation-checked `ThreadId` and a monotonically checked per-slot wait
  sequence. The sequence allocator survives slot reclaim; overflow panics
  before a token is published.
- Each occupied scheduler slot stores bounded inline wait metadata in one of
  `None`, `Prepared`, `Blocked`, or `Completed`. `PreparedWait` is a non-copyable
  single-use registration consumed by commit or cancellation.
- Scheduler lifecycle uses generic `TaskState::Blocked`. `WaitKind` is a coarse
  diagnostic classification only and never carries owner identity or payload.
- Wait publication follows this order:

  ```text
  exclude local IRQ interleaving
    -> lock the condition owner
    -> recheck the condition
    -> prepare scheduler wait
    -> publish WaitToken with the owner
    -> publish an optional WaitDeadline(token)
    -> release the owner lock
    -> commit the prepared wait
  ```

  The controlled exception entry remains IRQ-masked through commit, but no
  owner lock crosses scheduler context selection.
- Completion follows the inverse lock order: claim a token under the owner
  lock, release that lock, then call `complete_wait`. The scheduler never calls
  back into an external owner while changing wait or runnable state.
- Completion of `Prepared` records the cause without blocking. Commit consumes
  that early cause and keeps the current task running. Completion of `Blocked`
  performs the centralized `Blocked -> Ready` transition and inserts one ready
  entry. Completion never switches directly; existing deferred-reschedule
  checkpoints own optional handoff.
- The first exact completion wins. A duplicate exact token returns
  `AlreadyCompleted`; a different generation, sequence, or active token returns
  `Stale`. Neither result changes the retained cause or ready queue.
- Kernel task-call requests retain the exact token on the caller's stack. After
  the saved context resumes past SVC, the wrapper calls `finish_wait(token)` to
  consume the cause. Restarted userspace waits retain the token in their
  process or console owner state until the repeated syscall consumes it.
- Time stores `WaitDeadline(WaitToken)` and completes it with `Timeout`.
  Mailbox queues store the same token. An event winner claims its mailbox token,
  cancels the deadline, and completes `Notified`; a timeout winner is removed
  from the mailbox queue by the resumed owner. Bounded wake scanning skips
  already-completed or stale loser tokens.
- Thread lifecycle retains join relationships and exit results; process
  lifecycle retains parent relationships and terminal status; console retains
  RX bytes. Their registrations contain `WaitToken`, while generic scheduler
  wait metadata contains only token, kind, lifecycle, and cause.
- Prepare, commit, completion, finish, cancellation, state/queue mutation, and
  invariant validation remain bounded and allocation-free. The implementation
  remains single-core and local IRQ exclusion is not SMP synchronization.

## Invariants

- A task is scheduler-blocked if and only if its inline wait state is
  `Blocked` for one exact token.
- `Prepared` belongs only to the current `Running` task. `Completed` belongs
  only to a `Ready` or `Running` task until exact consumption.
- A new wait cannot begin before the previous completion is consumed or its
  preparation is cancelled.
- External owners store exact tokens and payloads separately. Scheduler code
  never stores mailbox data, exit status, process status, or UART bytes.
- Owner locks are released before wait commit or completion. Scheduler code
  never acquires an external owner lock.
- A late deadline or event cannot complete a later wait or a reused thread
  slot, and duplicate completion cannot create a duplicate ready entry.

## Partial supersession

- [ADR-0006](ADR-0006-time-owned-timed-events.md): timed task wakeups now carry
  exact wait identity rather than only task identity; time retains deadline
  ownership and callback-free dispatch.
- [ADR-0012](ADR-0012-bounded-mailbox-ipc.md): mailbox waiter queues store
  `WaitToken`, and owner locks are released before scheduler completion or
  commit.
- [ADR-0013](ADR-0013-mailbox-timeout-semantics.md): generic deadline
  completion replaces scheduler-owned IPC cleanup identity; mailbox owns loser
  removal.
- [ADR-0014](ADR-0014-bounded-kernel-thread-lifecycle.md): join registration
  uses an exact wait token while lifecycle retains exit-code ownership.
- [ADR-0017](ADR-0017-process-table-and-user-fault-policy.md): process join and
  child wait registrations use exact tokens while process status remains in the
  process table.
- [ADR-0020](ADR-0020-uart-stdin-and-shell.md): console stores an exact token
  instead of a bare thread identity, and `StdinRead` is no longer a scheduler
  block reason.
- [ADR-0031](ADR-0031-centralized-scheduler-state-transitions.md):
  `Blocked(reason)` and specialized thread-generation wake validation are
  replaced by generic `Blocked` plus scheduler-owned exact wait metadata. Its
  centralized transition and context-handoff boundaries remain in force.

## Consequences

The prepare/publish/commit protocol closes wake-before-block without retaining
an owner lock across scheduler handoff. Event/timeout arbitration has one
auditable first-wins operation, and stale completions become expected input
rather than lifecycle corruption.

Kernel task-call request storage gains a private mutable output, but the EL0
syscall ABI and architecture frame layout do not change. Timed mailbox cleanup
is intentionally owner-driven after timeout; wake paths may inspect at most the
preallocated waiter capacity while skipping loser tokens.

The design does not add multiple-object waits, futures, cancellation as a third
completion cause, priority policy, SMP synchronization, signals, or TTY
semantics.

## Alternatives considered

- Keep specialized block reasons and wake entry points: rejected because they
  duplicate lifecycle logic and cannot identify successive waits by one live
  thread.
- Put mailbox/process/console identity or payload in the scheduler token:
  rejected because it reverses ownership and creates lock-order callbacks.
- Hold owner locks through commit: rejected because scheduler selection must
  not carry an external condition borrow into another runnable context.
- Let timeout dispatch call mailbox cleanup directly: rejected because time and
  scheduler would need mailbox identity and an inverse owner-lock dependency.
- Allocate wait objects dynamically: rejected because wait registration and
  completion are scheduler/IRQ fast paths with bounded preallocated storage.

## Validation

- Unit-test normal, early, cancelled, duplicate, stale sequence/generation,
  unfinished, and overflow wait-state paths without an architecture context.
- Contract-test wake-before-block, duplicate blocked completion, both
  event/timeout orders, late timeout against a later wait, slot reuse, and
  timed mailbox loser cleanup.
- Retain the complete sleep, mailbox, thread join, process wait, stdin,
  userspace, shell, and user-fault QEMU contracts.
- Structurally reject device-specific block reasons, slot-only wake APIs, and
  external wait queues containing bare thread/task identity.

## Related decisions

- [ADR-0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md)
- [ADR-0029](ADR-0029-local-irq-and-task-preemption-exclusion.md)
- [ADR-0030](ADR-0030-nested-preemption-control-and-deferred-rescheduling.md)
- [ADR-0031](ADR-0031-centralized-scheduler-state-transitions.md)
