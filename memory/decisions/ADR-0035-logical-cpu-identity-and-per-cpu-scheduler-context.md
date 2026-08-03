# ADR-0035: Logical CPU identity and per-CPU scheduler context

## Status

Accepted

## Context

The active AArch64 target starts only the QEMU `virt` boot CPU. Secondary CPUs
remain in the low boot trampoline's `WFE` loop, but scheduler execution-local
state was held in global singleton fields. That made current-thread identity,
idle selection, ready membership, scheduler availability, and preemption
ownership structurally single-core rather than explicitly CPU-local.

`PreemptGuard` is also used by the heap before scheduler bootstrap allocates its
thread table. CPU-local preemption state therefore cannot be introduced only in
heap-allocated scheduler storage.

## Decision

- Generic kernel code uses typed `CpuId`; only the bounded CPU registry creates
  logical IDs. CPU0 is the boot CPU and raw hardware identities are never used
  as scheduler indices.
- AArch64 alone reads `MPIDR_EL1` and normalizes Aff3:Aff0 into an opaque
  hardware key. Boot registration maps that key to CPU0 before memory and
  scheduler initialization, then binds the logical index to that CPU through
  software-owned `TPIDR_EL1`. The boot trampoline clears this binding on every
  CPU before secondary CPUs park.
- The fixed-capacity registry rejects duplicate and overflow registrations.
  Registration may scan the bounded hardware-key set, while runtime current-CPU
  resolution reads and validates the CPU-local logical binding in O(1).
  Unbound or invalid CPUs are rejected rather than assuming CPU0.
- `PreemptGuard` carries its creating CPU. Before scheduler publication it uses
  fixed per-CPU boot backing so heap initialization remains possible; after
  publication it accesses the preemption state owned by that CPU's scheduler
  context. A guard may not cross publication, and dropping it on another CPU
  is an invariant panic.
- Scheduler bootstrap preallocates one bounded `CpuSchedulerState` per logical
  CPU. Each context owns local current, idle, ready queue, initialized/online
  flags, and its preemption state. The global scheduler
  retains the shared bounded thread table, free slots, stacks, and immutable
  configuration.
- `ThreadAttrs` selects `Current` or an explicit immutable CPU affinity before
  publication. Each thread records `home_cpu`; publication and wakeup queue it
  only on that CPU. Explicit unregistered or offline affinity is a controlled
  spawn error. There is no migration.
- Public scheduler entry points determine the current CPU once and bind a
  short-lived `CpuScheduler` view. Local transition, wait, thread, and
  preemption operations use that stable view without repeatedly resolving or
  passing `CpuId`. Bootstrap, explicit affinity, home-CPU wakeup, and validation
  select a target context explicitly. CPU0 becomes online only immediately
  before entering the first selected context. All other contexts remain
  offline.

## Invariants

- Only CPU0 executes kernel code in this milestone. Local IRQ exclusion and all
  existing `unsafe impl Sync` comments remain single-active-CPU assumptions,
  not SMP synchronization.
- No CPU binding lookup, preemption operation, scheduler transition, IRQ
  handoff, ready operation, or wakeup allocates. Current-CPU resolution is O(1),
  and runtime queue capacity is reserved at bootstrap.
- A running or queued thread belongs to its immutable home CPU. The idle thread
  is selected from the local CPU context, never from a fixed slot number.
- Secondary contexts are allocated but are neither registered nor initialized,
  online, interrupted, timed, or scheduled.

## Consequences

The current single-core behavior remains CPU0-only while scheduler ownership is
ready for future CPU activation. A future implementation must add secondary
boot registration, synchronization for shared kernel state, timer/interrupt
ownership, remote wake notification/IPIs, and any migration policy before it
can make another context online.

## Alternatives considered

- Keep global scheduler/preemption singletons: rejected because it hides the
  ownership boundary required before SMP activation.
- Allocate all CPU-local state with the scheduler: rejected because heap uses
  `PreemptGuard` before scheduler bootstrap.
- Treat MPIDR as a scheduler index: rejected because hardware affinity values
  are architecture identifiers, not logical kernel IDs.
- Add SMP locks, IPIs, migration, or secondary entry now: rejected because they
  exceed the CPU0-only milestone and would require a separate shared-state
  synchronization design.

## Validation

- Unit-test bounded registry CPU0 assignment, duplicate rejection, and
  overflow; contract-test stable O(1) logical binding reads.
- Contract-test CPU0 registry/context state and inactive secondary contexts via
  `GTRT/1` kernel cases, alongside existing scheduler, wait, and preemption
  contracts.
- Run formatting, host checks, the targeted QEMU contract, and canonical CI.

## Related decisions

- [ADR-0001](ADR-0001-architecture-strategy.md)
- [ADR-0003](ADR-0003-aarch64-preemptive-irq-return-switching.md)
- [ADR-0011](ADR-0011-dynamic-preallocated-scheduler-and-time-structures.md)
- [ADR-0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md)
- [ADR-0030](ADR-0030-nested-preemption-control-and-deferred-rescheduling.md)
- [ADR-0033](ADR-0033-unified-thread-model-and-scheduler-process-separation.md)
