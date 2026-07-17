# ADR-0031: Centralized scheduler state transitions

## Status

Accepted

## Context

The bounded single-core scheduler already had typed saved-context ownership and
deferred rescheduling checkpoints, but lifecycle mutation was distributed
across bootstrap, preemption, blocking, IPC, and thread code. Those paths wrote
task state, ready-queue membership, current identity, and slot generation
separately. Each path was individually IRQ-excluded, yet the ownership rule was
implicit and difficult to audit for stale generations, duplicate queue entries,
or partially published transitions.

Context handoff also sat next to those mutations. The architecture-neutral
scheduler needs to choose a task without giving lifecycle code access to frame
representation, while the handoff path needs a stable decision without
reimplementing state changes.

## Decision

- A private `kernel::sched::transition` module is the sole writer of task
  lifecycle state, slot generation, current-task identity, ready-queue
  membership, and saved-context occupancy associated with publication/reclaim.
- The allowed lifecycle edges are:

  ```text
  Free -> Ready
  Ready -> Running
  Running -> Ready
  Running -> Blocked(reason)
  Blocked(reason) -> Ready
  Running -> Zombie
  Zombie -> Free
  ```

  Detached exit may complete the final two edges within one mandatory handoff.
- Ready-queue entries carry complete `ThreadId { index, generation }` identity.
  Publication advances generation before enqueue; dequeue and reclaim reject
  stale or duplicate slot identities.
- Sleep wakeups, quantum expiration, and IPC timeouts also retain `ThreadId`
  generation identity until dispatch. The slot-only compatibility wake is
  limited to sleep, runs under local IRQ exclusion, and cancels the exact timed
  event before waking. Other block reasons retain cleanup ownership in their
  subsystem. Scheduler-owned timed events never synthesize a new generation
  from an old slot index.
- Invalid internal transitions are kernel invariant failures and panic.
  Duplicate, late, or mismatched external wake notifications return without
  changing scheduler state.
- Transition selection returns a context-free `SwitchOutcome`. Preemption and
  thread handoff code owns `ActiveContext`/`SavedContext` save and restore,
  address-space activation, timer quantum replacement, and switch logging.
- The transition layer does not consume deferred reschedule state. Optional
  checkpoints consume one coalesced request before asking for a transition;
  requeueing the outgoing task does not manufacture another request.
- Debug and `qemu-test` builds run a bounded, allocation-free invariant walk
  after transitions. It verifies current/running uniqueness, context occupancy,
  free-slot cleanliness, ready membership, and generation validity. The public
  test seam masks local IRQs for the duration of the walk.
- Round-robin selection, wake ownership, blocking semantics, capacities,
  quantum, and architecture context ABI remain unchanged.

## Invariants

- Exactly one task is `Running` after initial dispatch, and it is exactly the
  generation-bearing identity stored in `current`.
- Every non-idle `Ready` task appears exactly once in the ready queue. No other
  task appears there, and the idle task is never queued.
- `Free` slots own no saved context, join metadata, IPC result, user-thread
  kind, or queue/current identity.
- An occupied slot owns one saved context until reclaim.
- A stale `ThreadId` cannot wake, join, dequeue, or reclaim a reused slot.
- State/queue/current mutation remains bounded and allocation-free after
  scheduler bootstrap.

## Consequences

Lifecycle correctness has one auditable mutation boundary, and QEMU contracts
can validate the complete scheduler table after controlled transition cycles.
Generation-bearing queue entries make stale slot reuse visible immediately
instead of silently selecting a different thread.

The transition module is intentionally coupled to the private scheduler table;
it is not a reusable public state-machine API. Wake producers still own their
external wait queues and deadlines, and context handoff still requires a short
architecture-neutral orchestration step after transition selection.

The validator is diagnostic coverage, not production scheduling work. Release
builds rely on transition construction and fail-fast checks rather than paying
for a full table walk after every edge.

## Alternatives considered

- Keep mutations in each owning scheduler submodule: rejected because state,
  queue, and current updates could not be audited as one operation.
- Put context save/restore inside the transition module: rejected because it
  would mix architecture context ownership with lifecycle state selection.
- Store only `TaskId` indexes in the ready queue: rejected because slot reuse
  would erase the identity needed to detect stale queue ownership.
- Treat impossible internal transitions as no-ops: rejected because this would
  hide kernel corruption. Tolerance is reserved for externally stale wakeups.
- Introduce a generic dynamic state-machine framework: rejected because the
  finite private transition table is smaller, bounded, and easier to audit.

## Validation

- Exercise repeated ready/running/block/wake/zombie/free cycles in the AArch64
  kernel contract.
- Repeatedly request scheduling while a peer remains ready and validate that it
  has one queue entry and executes once.
- Consume a deadline wake, submit duplicate and late wake notifications, and
  verify one execution.
- Reuse a reclaimed slot and prove that the stale generation is rejected.
- Run deferred-preemption cases with invariant checks while handoff is held and
  after the depth-zero checkpoint.
- Structurally search for lifecycle, current, generation, and ready-queue writes
  outside `sched::transition`.

## Related decisions

- [ADR-0003](ADR-0003-aarch64-preemptive-irq-return-switching.md)
- [ADR-0006](ADR-0006-time-owned-timed-events.md)
- [ADR-0011](ADR-0011-dynamic-preallocated-scheduler-and-time-structures.md)
- [ADR-0014](ADR-0014-bounded-kernel-thread-lifecycle.md)
- [ADR-0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md)
- [ADR-0030](ADR-0030-nested-preemption-control-and-deferred-rescheduling.md)
