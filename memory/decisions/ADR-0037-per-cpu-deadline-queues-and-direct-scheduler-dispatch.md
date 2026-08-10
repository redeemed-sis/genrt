# ADR-0037: Per-CPU deadline queues and direct scheduler dispatch

## Status

Accepted

## Context

The logical-CPU scheduler and SMP synchronization work made scheduler execution
state CPU-local, but kernel::time still owned one global deadline queue and one
software armed_timer_deadline. AArch64 CNTP_* state is local to the executing
PE, so a shared software queue could not correctly represent multiple physical
timers even when protected by an SMP lock.

Time also dispatched typed events through a callback table injected by
scheduler bootstrap. Both subsystems already depended on each other's types and
operations, so the table obscured rather than enforced the ownership boundary.

## Decision

- Allocate one bounded TimeState slot per logical CPU. Each slot owns its
  deadline queue, software armed deadline, and IRQ-dispatch state.
- Initialize a CPU's queue only after that CPU's scheduler state is published.
  Runtime queue storage is fully reserved at initialization.
- Every TimedEvent carries its logical CPU owner. A wait token captures the
  immutable home CPU of the waiting thread; a quantum event captures the
  CpuScheduler owner.
- Only the executing owner CPU may schedule an event or program its architected
  timer. Remote event insertion fails fast until an IPI-backed remote timer
  command exists.
- Exact cancellation may remove an event from another CPU's synchronized queue.
  It does not program that CPU's timer. The already-armed earlier deadline may
  therefore cause one harmless interrupt, after which the owner observes the
  updated queue and rearms locally.
- Timer IRQ dispatch pops one event while holding only the local queue lock,
  releases it, and calls the narrow sched facade directly. kernel::time invokes
  exact wait completion, quantum expiration, and final IRQ-return scheduling
  without an injected callback table.
- The time subsystem continues to own deadline ordering and timer programming;
  the scheduler continues to own wait completion, runnable state, quantum
  policy, and context handoff.

## Invariants

- One CPU never programs another CPU's architected physical timer.
- An event is inserted only into the queue named by its immutable owner.
- Deadline queue capacity is fixed before runtime timer IRQs and no timed-event
  operation allocates.
- No time queue lock is held while entering the scheduler.
- Remote cancellation cannot delay a remaining deadline; at worst it leaves a
  stale early hardware deadline and one extra interrupt.
- Secondary CPU execution still requires local timer initialization plus IPI
  support for prompt remote event insertion and rescheduling.

## Consequences

CPU0 behavior and the one-shot nearest-deadline model remain unchanged. Fixed
storage now represents the architecture's per-PE timer ownership correctly and
can be initialized independently during future secondary bring-up.

The callback interface disappears. This is an intentional direct dependency on
the scheduler facade, not on scheduler internals.

This decision partially supersedes ADR-0005's system-wide single timer wording,
ADR-0006's single queue and injected handler set, and ADR-0036's classification
of the deadline queue as one shared runtime owner.

## Alternatives considered

- Keep one globally locked queue: rejected because one nearest deadline cannot
  describe several independently programmed PE-local timers.
- Let any CPU mutate and reprogram another CPU's timer: rejected because the
  AArch64 physical timer registers are local to the executing PE.
- Retain callbacks for layering: rejected because typed time events already
  depend on scheduler identities and callbacks provided no independent
  interface or ownership boundary.
- Add IPI remote scheduling now: deferred to secondary CPU activation because
  no secondary CPU currently executes kernel work.

## Validation

- Host tests verify that equal thread identities owned by different CPUs remain
  distinct and isolated in separate deadline queues.
- The AArch64 kernel contract exercises wait deadlines, mailbox timeout,
  quantum expiration, timer preemption, and deferred rescheduling on CPU0.
- Formatting, host tests, AArch64 build checks, rustdoc, and canonical CI cover
  the cross-subsystem integration.

## Related decisions

- [ADR-0005](ADR-0005-one-shot-timer-deadline-engine.md)
- [ADR-0006](ADR-0006-time-owned-timed-events.md)
- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [ADR-0035](ADR-0035-logical-cpu-identity-and-per-cpu-scheduler-context.md)
- [ADR-0036](ADR-0036-smp-safe-runtime-synchronization.md)
