# ADR-0029: Separate local IRQ exclusion from task preemption exclusion

## Status

Accepted

## Context

The single-core kernel used one `IrqSpinLock` name for two different ownership
contracts. Mailbox control is shared between task and timer/timeout paths, while
the kernel heap and physical frame allocator are task-context resources that
only need protection from task preemption. The implementation masked local IRQs
and treated contention as a bug; it never spun and provided no SMP guarantee.

Keeping one IRQ-specific type for both contracts made call sites claim more
sharing than their owners intended and made a later IRQ-enabled deferred
preemption implementation harder to introduce safely. This change must not
alter the established IRQ-return scheduling mechanics.

## Decision

- `LocalIrqGuard` and `LocalIrqLock<T>` protect short state transitions shared
  with interrupt handlers. They save and mask local IRQ state and restore it on
  guard drop.
- `PreemptGuard` and `PreemptLock<T>` protect mutable state used only from
  bootstrap or ordinary task context. Acquiring them from IRQ context is
  forbidden.
- The current `PreemptGuard` backend delegates to `LocalIrqGuard`. This preserves
  existing scheduling behavior while separating ownership semantics at call
  sites.
- Both lock types detect recursive or contended entry and panic. Neither spins,
  allocates, blocks, or provides SMP synchronization.
- Mailbox control remains under `LocalIrqLock`. Scheduler, timer/deadline,
  process, console RX, and test-protocol state retain their existing local-IRQ
  exclusion or exception-entry discipline in this change.
- The fixed kernel heap and runtime physical frame allocator use
  `PreemptLock`. Heap and frame allocation remain forbidden from IRQ,
  scheduler-handoff, and timed-event fast paths.
- Boot-discovered physical memory metadata becomes immutable after
  initialization. Only runtime free-list state is placed under `PreemptLock`,
  and no reference into that state escapes a guard.
- Deferred rescheduling with IRQs enabled, a preemption counter, process/VM lock
  migration, and SMP locks are separate future decisions.

## Invariants

- State reachable from both task and IRQ context uses local IRQ exclusion.
- `PreemptLock` protects task-only state and is never acquired by an interrupt
  handler.
- The transitional preemption backend preserves the previous local IRQ masking
  and IRQ-return scheduling semantics.
- Lock guards are neither `Copy` nor `Clone`, and protected borrows cannot
  outlive their guards.
- Runtime frame allocator mutation is serialized; immutable memory-map metadata
  remains available without holding the allocator lock.
- Heap and frame allocator critical sections do not allocate recursively or
  block.
- No primitive introduced here claims SMP safety.

## Partial supersession

- [ADR-0010](ADR-0010-irq-safe-kernel-heap-lock-and-allocation-policy.md):
  replaces the heap lock's IRQ-shared naming and ownership contract with a
  task-only `PreemptLock`. The fixed heap, allocation policy, OOM behavior, and
  prohibition on allocation in fast paths remain unchanged.
- [ADR-0012](ADR-0012-bounded-mailbox-ipc.md): replaces only the obsolete
  `IrqSpinLock` name with `LocalIrqLock`. Bounded mailbox storage, wait queues,
  timeout cleanup, and scheduler handoff remain unchanged.

## Consequences

Call sites now state whether IRQ sharing or task preemption is the reason for a
critical section. The heap and frame allocator can later adopt true deferred
preemption without changing their ownership API, while IRQ-shared state retains
an explicit local IRQ contract.

The heap retains its previous IRQ-masked latency. Runtime frame allocation now
also masks IRQs while walking or mutating the free list, closing a reachable
task-preemption race at the cost of additional IRQ latency. The implementation
does not yet permit IRQ delivery inside task-only critical sections, and it
does not add cross-core exclusion; allocator latency measurement and redesign
remain future hardening work.

## Alternatives considered

- Keep `IrqSpinLock`: rejected because the implementation does not spin and the
  name erases the task-only ownership distinction.
- Make `PreemptGuard` IRQ-enabled now: rejected because that requires a
  preemption counter, deferred reschedule semantics, and scheduler changes
  outside this hardening step.
- Put all physical memory metadata under `PreemptLock`: rejected because the
  boot map is immutable at runtime and exposing references through a mutable
  allocator guard would unnecessarily widen lock scope.
- Migrate process, VM, scheduler, and timer state together: rejected because it
  would combine ownership classification with scheduler semantic changes.

## Validation

- Build and post-link check the production AArch64 kernel.
- Run the kernel contract with bounded joinable workers that allocate and free
  physical frames and heap-backed vectors, then verify the free-frame count is
  restored.
- Run every existing AArch64 QEMU contract and the canonical CI gate.
- Audit that `IrqSpinLock` no longer exists and classify all `LocalIrqLock`,
  `LocalIrqGuard`, `PreemptLock`, and `PreemptGuard` call sites.
- Run formatting, xtask unit tests/clippy, kernel rustdoc, and
  `git diff --check`.

## Related decisions

- [ADR-0003](ADR-0003-aarch64-preemptive-irq-return-switching.md)
- [ADR-0005](ADR-0005-one-shot-timer-deadline-engine.md)
- [ADR-0006](ADR-0006-time-owned-timed-events.md)
- [ADR-0007](ADR-0007-dtb-memory-map-and-frame-allocator.md)
- [ADR-0009](ADR-0009-bootstrap-kernel-heap-on-frame-allocator.md)
- [ADR-0010](ADR-0010-irq-safe-kernel-heap-lock-and-allocation-policy.md)
- [ADR-0011](ADR-0011-dynamic-preallocated-scheduler-and-time-structures.md)
