# ADR-0055: Batched timer wakeup and direct idle selection

## Status

Accepted

Supersedes [ADR-0037](ADR-0037-per-cpu-deadline-queues-and-direct-scheduler-dispatch.md)
only for per-event timer-to-scheduler dispatch. CPU-local queue ownership,
direct typed dispatch, and remote insertion restrictions remain unchanged.

## Context

Completing each expired wait through a separate scheduler acquisition, then
acquiring the scheduler again to select the return thread, adds work to common
periodic wakeups. A sole user thread waking from idle also does not need a
ready-queue round trip or a new RR quantum. These optimizations must retain
exact wait identities, deferred preemption, and the normal multi-peer policy.

## Decision

- CPU0 preallocates one fixed expired-event buffer per time state with the
  same bound as that CPU's deadline queue. Timer IRQ extracts all events
  expired at its sampled counter into the initialized prefix of this buffer.
- Transfer buffer ownership out of the time lock before scheduler entry. One
  scheduler transaction finalizes retired joins, completes the local batch,
  accounts quantum expiration, selects a thread, and hands off its context.
- Timer-owned completions publish readiness and coalesced reschedule requests
  without acquiring time ownership. This includes locally completed retired
  joiners. Remote completions retain their target-owned ingress and IPI path.
- Return bounded obsolete/ensure quantum intent. Apply it and return the
  buffer under the time owner only after scheduler ownership releases, then
  rearm the owner's physical timer. Existing exact quantum deadlines are not
  extended just because an unrelated timer event expired.
- With exactly one expired local wait, current permanent idle, empty local
  ready queue, empty remote ingress, and enabled preemption, complete and
  select that thread directly in the transition layer. Preserve normal
  Blocked-to-Ready-to-Running states without publishing a queued intermediate.
- Use ordinary wait completion for generation, CPU, sequence, first-wins,
  explicit completion retention, and automatic EL0 consumption. Stale or
  already completed waits never manufacture a second runnable thread.
- Direct selection consumes the depth-zero checkpoint and saves/restores
  the existing contexts. No quantum is created when no peer remains. Multiple
  events, existing peers, or guarded execution use the normal batched RR path.

## Invariants

- No time owner crosses timer scheduler ownership; no scheduler owner crosses
  the final quantum update or timer rearm.
- Buffers and queues never grow in IRQ context. Work is bounded by configured
  per-CPU event and thread capacities; stack usage is not a capacity-sized
  temporary event array.
- Every expired registration still obeys exact-token first-wins completion.
  Cancellation after extraction may leave a stale copied event, not a wake of
  a later registration or reused thread slot.
- Current identity, lifecycle states, and saved-context ownership still change
  only through existing scheduler transition and handoff boundaries.
- Deferred rescheduling cannot switch under a preemption guard.

## Consequences

The common periodic idle wake takes one scheduler transaction and no queue
round trip. Bootstrap reserves one additional bounded event buffer per CPU.
Normal RR semantics and owner-local timer programming are unchanged.

Exact idle context saving remains intentional. Idle entry currently accepts an
arbitrary function and can retain locals, so it is not contracted restartable.
Skipping its save would require a separate entry/lifetime decision rather than
assuming it is a disposable loop. No polling or spin wait is added.

## Alternatives considered

- Holding the time lock while completing waits introduces a lock-order cycle.
- A capacity-sized stack batch increases interrupt stack pressure.
- A benchmark-specific wake path would bypass generic wait and lifecycle rules.
- Replacing idle with a restart stub would change its entry contract without
  evidence that exact-context saving is the remaining bottleneck.

## Validation

Host tests cover fixed buffer limits, single ownership, future deadlines,
storage reuse, exact explicit/automatic wait consumption, stale registration,
and queued/remote peers. The kernel EL0 probe requires 32 direct handoffs per
lifetime. Existing QEMU contracts cover duplicate and late completion, mailbox
timeout races, guarded wakeup, non-yielding RR, and SMP owner-local sleeps.
Hardware latency remains a separate measured result, not a QEMU assertion.

## Related decisions

- [ADR-0037](ADR-0037-per-cpu-deadline-queues-and-direct-scheduler-dispatch.md)
- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [ADR-0031](ADR-0031-centralized-scheduler-state-transitions.md)
- [ADR-0054](ADR-0054-idle-user-root-retention-and-detach.md)
