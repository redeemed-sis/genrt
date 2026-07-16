# ADR-0030: Nested preemption control and deferred rescheduling

## Status

Accepted

## Context

ADR-0029 separated local IRQ exclusion from task-preemption exclusion, but its
transitional `PreemptGuard` backend held local IRQs masked for the complete
critical section. That preserved scheduler behavior while ownership was made
explicit, at the cost of stopping timer and device interrupt progress during
heap and frame allocator operations.

The active scheduler commits optional switches by replacing a typed
`ActiveContext` at controlled exception entries. A task-only critical section
therefore needs to prevent task handoff without suppressing IRQ bookkeeping or
retaining a raw context pointer in its guard.

## Decision

- `kernel::sync::preempt` owns a bounded single-core state containing a nested
  disable depth, a coalescing reschedule-pending flag, and scheduler-online
  state. The mutable state is private and each update occurs under a short
  `LocalIrqGuard`.
- `PreemptGuard` increments and decrements the disable depth without retaining
  an IRQ guard. Timer and device IRQs remain enabled when the caller entered
  with IRQs enabled.
- Only release of the outermost guard may initiate a deferred checkpoint. It
  first restores the caller's prior IRQ state and uses the private EL1 task-call
  path; it never switches directly from `Drop` and never stores an
  `ActiveContext` or raw frame pointer.
- `PreemptLock` clears its ownership flag before releasing its
  `PreemptGuard`. A task selected by the deferred checkpoint therefore cannot
  observe a lock still owned by the switched-out task.
- Scheduler policy requests rescheduling by setting the coalescing pending
  flag. Timer IRQ return and the private `PreemptCheckpoint` task call are the
  optional scheduler checkpoints that may consume it when disable depth is
  zero.
- Quantum expiration and idle wakeup continue all timer/deadline bookkeeping
  while preemption is disabled. They leave the current task `Running`, preserve
  ready-queue membership, and retain the pending request for a later safe
  checkpoint.
- Kernel `yield` requests rescheduling and enters the same controlled
  checkpoint. Under a guard it returns to the current task; the switch occurs
  after the outermost guard is released.
- Blocking and terminal task transitions are forbidden while disable depth is
  nonzero. Sleep, blocking joins/waits, mailbox waits, stdin waits, and
  thread/process exit fail fast before publishing wait or terminal ownership.
- If the outermost guard is released while an enclosing context already has
  local IRQs disabled, it does not issue an SVC. The pending request remains for
  the next timer IRQ-return or explicit controlled scheduler entry.
- The mechanism is single-core only. It is not a spinlock, does not provide SMP
  exclusion, and does not change `ActiveContext` or `SavedContext` layout.

## Invariants

- A context switch is never committed while preemption disable depth is
  nonzero.
- IRQ handlers may update bounded timer, deadline, wakeup, and ready state while
  task preemption is disabled, but may not allocate, block, or acquire a
  `PreemptLock`.
- Reschedule requests coalesce and are cleared only by a scheduler checkpoint.
- Nested guard release cannot checkpoint until the outermost guard is released.
- Lock ownership is released before a deferred checkpoint can run another
  task.
- Mandatory blocking or terminal handoff checks preemption state before any
  externally visible waiter or lifecycle transition.
- Local IRQ state is restored exactly; preemption control never enables IRQs
  that were disabled by its caller.

## Partial supersession

- [ADR-0029](ADR-0029-local-irq-and-task-preemption-exclusion.md): replaces the
  transitional `PreemptGuard` backend that masked local IRQs for the complete
  task-only critical section. ADR-0029's ownership split, lock classification,
  mailbox `LocalIrqLock`, allocator `PreemptLock`, fail-fast non-spinning lock
  policy, and lack of SMP guarantees remain in force.

## Consequences

Heap and runtime frame allocator critical sections no longer stop timer or
device IRQ delivery. Their task-only mutation remains protected because timer
IRQ return defers any requested switch until the owning task releases its
outermost guard.

Guard exit may synchronously cross the private EL1 task-call boundary when a
switch is pending and the caller's IRQ state permits it. This checkpoint is
bounded and allocation-free, but its context-switch cost is now part of the
outermost guard release path. Blocking under a guard becomes an explicit kernel
bug instead of silently handing protected state to another task.

The design does not certify allocator latency, add priority scheduling, or
provide cross-core synchronization. Centralizing all scheduler state
transitions remains separate hardening work.

## Alternatives considered

- Keep IRQs masked for the full task-only critical section: rejected because it
  prevents timer/deadline progress and defeats the ownership distinction made
  by ADR-0029.
- Switch directly from `PreemptGuard::drop`: rejected because the guard has no
  live typed exception context and scheduler handoff must remain at controlled
  architecture entries.
- Store an `ActiveContext` or raw trap-frame pointer in the guard: rejected
  because it would violate the typed live-context lifetime boundary.
- Defer the complete timer handler: rejected because monotonic time, deadlines,
  wakeups, and hardware rearming must continue while task preemption is
  disabled.
- Silently defer blocking operations: rejected because wait registration and
  protected-resource ownership cannot safely survive an unspecified later
  handoff.
- Introduce an SMP spinlock or per-task preemption state: rejected because the
  active target is single-core and SMP is outside this decision.

## Validation

- Contract-test that timer IRQs execute while a `PreemptLock` is held and that
  a runnable peer cannot execute until unlock.
- Contract-test direct, nested, yield-triggered, and deadline-wakeup deferred
  rescheduling without an explicit post-unlock yield.
- Repeat bounded heap/frame/thread lifecycle operations under timer activity and
  verify frame counts, joins, and post-test liveness.
- Run the complete AArch64 QEMU contract suite, host checks, formatting, clippy,
  rustdoc, and structural searches for preemption state and checkpoint paths.

## Related decisions

- [ADR-0003](ADR-0003-aarch64-preemptive-irq-return-switching.md)
- [ADR-0005](ADR-0005-one-shot-timer-deadline-engine.md)
- [ADR-0006](ADR-0006-time-owned-timed-events.md)
- [ADR-0010](ADR-0010-irq-safe-kernel-heap-lock-and-allocation-policy.md)
- [ADR-0011](ADR-0011-dynamic-preallocated-scheduler-and-time-structures.md)
- [ADR-0012](ADR-0012-bounded-mailbox-ipc.md)
- [ADR-0027](ADR-0027-typed-active-context-and-syscall-boundary.md)
- [ADR-0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md)
- [ADR-0029](ADR-0029-local-irq-and-task-preemption-exclusion.md)
