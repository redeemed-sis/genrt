# ADR-0036: SMP-safe runtime synchronization and publication

## Status

Accepted

## Context

ADR-0035 made CPU identity, ready queues, and preemption ownership explicit,
but shared runtime state was still protected by `UnsafeCell` plus local IRQ
masking. That prevents local interrupt re-entry only, not concurrent CPUs.
Secondary startup remains out of scope, but it must not require another broad
replacement of shared-state ownership primitives.

## Decision

- `SpinLock<T>` is the generic raw AtomicBool spin primitive: successful
  acquisition is Acquire, retry polling is Relaxed with `spin_loop`, and unlock
  is Release. It owns a `PreemptGuard`; it is non-fair, allocation-free, and
  its guard is not `Send`.
- `IrqSpinLock<T>` saves/disables local IRQs before Acquire acquisition and
  releases before restoring IRQ state. It owns state reached from IRQ and
  normal context.
- `OncePublication<T>` publishes immutable-after-init state using Release and
  Acquire. Boot memory metadata uses it; ramfs and parsed platform data remain
  immutable after their boot publication.
- Shared scheduler lifecycle storage, the process table/reverse index, deadline
  queue, mailbox control, stdin owner, frame free list, heap metadata, and log
  serialization use cross-CPU locking. Per-CPU execution state and preemption
  bookkeeping remain CPU-local.
- Preemption bookkeeping uses one fixed per-CPU backing before and after
  scheduler publication. It remains outside scheduler storage so acquiring a
  `SpinLock` can enter `PreemptGuard` without recursively acquiring the
  scheduler lock. Scheduler publication requires this state to be quiescent but
  does not transfer its ownership or storage.
- Permitted nesting is condition owner (process/mailbox/stdin) -> scheduler ->
  time and kernel VM -> frame allocator; logging is terminal. CPU-registry and
  heap locks are standalone. The hierarchy is an ownership contract rather
  than a numeric rank embedded in every lock. Runtime lockdep is deferred until
  a per-CPU implementation can distinguish an interrupted lock stack from the
  interrupting context.
- A spinlock never contains blocking, scheduler handoff, user copy, parsing,
  heap allocation, logging bursts, or destructive cleanup. Owners unlock
  before completing waits/cleanup; time pops, unlocks, dispatches, and relocks.
- Runtime TTBR1 mappings remain mutable. The generic VM facade serializes
  writers; AArch64 keeps its descriptor-update and barrier/TLBI sequence local,
  uses break-before-make for protection changes, and reclaims page-table frames
  only after invalidation. TTBR roots are read from registers rather than
  mirrored in global software state.
- Logging is allocation-free. Producers enqueue complete records in a bounded
  TX ring under a short IRQ-safe lock; one drainer polls the UART outside that
  lock. Panic and test-abort output use an emergency raw path rather than
  waiting for normal serialization.
- GIC distributor mutation uses an IRQ-safe global lock; the CPU interface is
  initialized and operated locally.
  Secondary CPU interface initialization, IPI, migration, and userspace TLB
  shootdown remain non-goals.

## Ownership table

| State | Classification | Synchronization |
| --- | --- | --- |
| CPU binding, current thread, ready queue, preemption | CPU-local | owning CPU plus local IRQ masking |
| Scheduler thread table/free slots/remote ingress | shared runtime | scheduler `IrqSpinLock`; only target drains ingress |
| Process table/reverse index | IRQ/shared runtime | condition-owner `IrqSpinLock` |
| Deadline queue | IRQ/shared runtime | time `IrqSpinLock`; no callback while held |
| Mailbox/stdin | IRQ/shared runtime | condition-owner `IrqSpinLock`; completion after release |
| Frame free list / heap metadata | shared thread context | `SpinLock` |
| Memory metadata, ramfs, platform config | immutable after boot | Release/Acquire publication |
| TTBR1 mapping writer state | shared runtime | generic VM `SpinLock`; AArch64 descriptor and TLBI sequence |
| GIC distributor / CPU interface | global runtime / CPU-local | distributor `IrqSpinLock` / local interface policy |
| Logging / panic output | shared runtime | bounded TX queue and single drainer / non-waiting emergency path |

## Consequences

CPU0-only behavior remains unchanged; no migration or IPI is introduced. This
grows ADR-0029/0030 and partially supersedes ADR-0035's two-phase preemption
backing and scheduler-owned preemption-state decisions without rewriting that
historical record.

## Validation

- Host tests cover spinlock and allocator contention, mailbox completion,
  immutable publication, IRQ guard lifecycle, and remote-ready ownership.
- The kernel contract stays single-core and validates mapping/lifecycle policy
  across multiple kernel mapping blocks without claiming CPU1 execution.
- Formatting, host checks, the targeted kernel QEMU contract, and canonical CI
  are required for this cross-cutting change.

## Related decisions

- [ADR-0029](ADR-0029-local-irq-and-task-preemption-exclusion.md)
- [ADR-0030](ADR-0030-nested-preemption-control-and-deferred-rescheduling.md)
- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [ADR-0033](ADR-0033-unified-thread-model-and-scheduler-process-separation.md)
- [ADR-0035](ADR-0035-logical-cpu-identity-and-per-cpu-scheduler-context.md)
