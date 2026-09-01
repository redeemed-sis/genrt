# ADR-0040: Per-CPU scheduler activation

## Status

Accepted

## Context

ADR-0035 established one shared scheduler lifecycle table with fixed CPU-local
contexts. ADR-0038 brought every QEMU CPU through PSCI and generic identity
registration, and ADR-0039 completed local vectors, GICC, timer PPI, physical
timer, and generic time queues. The remaining CPU0-only scheduler boundary left
registered secondary contexts offline and parked despite having all prerequisites
for local scheduler entry.

Remote-ready ingress already has bounded target ownership, but this milestone
does not introduce an SGI/IPI or remote timer command. A post-online remote
publication therefore needs a deterministic target-local pickup mechanism.

## Decision

- Keep one shared bounded `Scheduler` and `ThreadTable` with fixed
  `CpuSchedulerState` storage. CPU0 preflights `KERNEL_THREAD_CAPACITY` for one
  permanent idle thread per expected CPU plus static bootstrap threads before
  publishing scheduler state.
- CPU0 behavior remains unchanged. Each secondary completes local AArch64
  GICC/PPI/timer setup before one generic secondary entry registers its logical
  identity and initializes its current-CPU scheduler context. Under existing
  scheduler ownership it asserts quiescent local preemption state, consumes one
  preallocated free slot, publishes a permanent nonjoinable idle thread with
  immutable local `home_cpu`, selects it as current, and marks the local context
  initialized but offline.
- The existing generic saved-context entry is the only secondary handoff. It
  marks scheduler-online immediately before `SavedContext::enter`, whose frame
  ABI installs the idle scheduler stack and unmasks IRQ. Successful secondary
  entry never returns to an architecture `WFI` park; initialization failure is
  recorded and hard parks with IRQ masked.
- CPU0 starts secondaries sequentially and waits boundedly for each exact
  registered `CpuId` to be scheduler-online. CPU registry records only
  identity/topology; parked fields and completion APIs are removed. Startup
  failure remains fatal rather than allowing partial availability.
- Every initialized CPU has one local idle thread and one local current running
  thread; online implies initialized. Scheduler validation counts one `Running`
  thread for every initialized CPU, while test validation requires every
  registered context to be online with local current/idle affinity.
- `ThreadAttrs` remains unchanged. Explicit remote affinity still requires the
  target CPU to be registered and online. There is no migration, work stealing,
  load balancing, IPI, SGI, remote preemption mutation, or remote timer
  insertion.
- Every online CPU retains a local RR quantum even without a runnable peer.
  Each timer checkpoint drains target-local remote-ready ingress and switches
  when work arrived; otherwise it rearms. Thus late remote affinity publication
  and cross-CPU completion execute within one RR quantum even when the current
  thread does not yield. This is an explicit bounded fallback, not immediate
  notification; Issue #7 owns the IPI replacement.
- Exit selection records the outgoing thread in one CPU-local retired slot.
  The slot and kernel stack remain occupied across the architecture return
  window and become reapable only at the next scheduler entry on the replacement
  stack. A racing late join may register against the retired exit but cannot
  publish its stack for reuse early.
- The four-CPU `smp-boot` contract validates all online contexts and idle
  ownership, parallel pinned workers on CPU1 and CPU2, timer RR of two
  non-yielding CPU1 workers, repeated local current/home checks, atomic
  reentrancy guards, cross-CPU joins, and recurring idle timer progress.

## Invariants

- CPU0 performs all allocation and capacity reservation before secondary PSCI
  startup. Secondary initialization, entry, scheduler handoff, timer dispatch,
  and remote-ingress drain allocate nothing.
- All lifecycle, current identity, and ready membership mutations pass through
  the scheduler transition layer. A free slot cannot become a secondary idle
  thread by any direct table mutation.
- A current thread's kernel stack cannot enter the free-slot pool before its
  owner CPU has entered the scheduler again on the replacement stack.
- Scheduler-online is published only at first saved-context entry, after the
  local context has initialized current/idle state and while AArch64 owns frame
  restore and IRQ unmasking.
- CPU-local timer ownership remains unchanged: target ingress pickup does not
  permit another CPU to insert a timer event or program another CPU's timer.
- Device SPIs and userspace remain CPU0-only. Secondary execution is limited to
  idle and explicitly pinned kernel threads.

## Consequences

Every registered CPU now reaches a running scheduler context, allowing bounded
pinned kernel work and target-local remote-ready pickup. The fallback latency is
one RR quantum rather than an IPI latency and imposes a periodic timer cost even
for a sole running thread, so it is suitable only for this intermediate
deterministic scope. Issue #7 must add notification before any immediate remote
scheduling claim.

This decision partially supersedes ADR-0035's single-active-context validation,
ADR-0038's parked scheduler boundary, and ADR-0039's scheduler-offline timer
return behavior. It preserves their CPU identity, PSCI, bootstrap-stack, local
interrupt ownership, per-CPU deadline ownership, and CPU0-only device/userspace
boundaries.

## Alternatives considered

- Keep secondaries parked: rejected because local scheduler prerequisites and
  bounded storage already exist, while it prevents testing the real ownership
  boundary.
- Add an IPI/SGI now: deferred to Issue #7 because notification protocol,
  acknowledgement, and failure handling are a separate lifecycle decision.
- Program remote timers for remote-ready work: rejected because only the owner
  CPU may program its physical timer and no remote command protocol exists.
- Allocate idle stacks during secondary startup: rejected because scheduler and
  secondary paths must remain allocation-free.

## Validation

- Host tests cover bootstrap idle-capacity preflight and secondary free-slot
  initialization.
- The four-CPU `smp-boot` QEMU contract covers online contexts, local idle
  affinity, actual concurrent pinned execution, timer RR, late remote ingress
  behind a non-yielding CPU3 worker, remote join ingress, and recurring local
  timer EOI progress.
- `cargo xtask check` and the targeted QEMU contract remain the applicable
  verification gate; `cargo xtask ci` is the canonical cross-cutting gate.

## Related decisions

- [ADR-0035](ADR-0035-logical-cpu-identity-and-per-cpu-scheduler-context.md)
- [ADR-0036](ADR-0036-smp-safe-runtime-synchronization.md)
- [ADR-0037](ADR-0037-per-cpu-deadline-queues-and-direct-scheduler-dispatch.md)
- [ADR-0038](ADR-0038-secondary-cpu-psci-boot-and-parked-state.md)
- [ADR-0039](ADR-0039-per-cpu-interrupt-and-timer-readiness.md)
