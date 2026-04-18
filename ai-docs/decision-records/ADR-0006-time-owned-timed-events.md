# ADR-0006: Time-Owned Timed Events

## Status
Accepted

## Context
`ADR-0005` moved `genrt` to a one-shot nearest-deadline timer model, but the
first implementation still kept deadline ownership in `kernel::sched`:
- sleeping tasks carried scheduler-owned deadlines,
- scheduler quantum was stored as scheduler state,
- nearest-deadline search and timer rearm logic also lived in the scheduler.

That worked functionally, but it blurred subsystem responsibilities and would
make future timeout-based IPC harder to extend cleanly.

## Decision
- Make `kernel::time` the only owner of timed events and timer rearm logic.
- Represent timeouts as typed events, not generic callbacks:
  - `WakeTask(task_id)`
  - `QuantumExpired(task_id)`
- Keep a fixed-size O(N) timed-event set in `kernel::time`.
- Let `kernel::time`:
  - register and cancel timed events,
  - find the nearest deadline,
  - program/disarm the one-shot timer,
  - collect expired events on timer IRQ,
  - dispatch those events through a fixed handler set injected during scheduler bootstrap.
- Let `kernel::sched` remain responsible only for:
  - task state transitions,
  - runnable selection,
  - block/wake semantics,
  - IRQ-return frame handoff.
  - round-robin quantum configuration during scheduler initialization.

## Rationale
This makes ownership explicit:
- time owns time,
- scheduler owns scheduling.

It also keeps the design simple for bring-up:
- no heap,
- no callback framework,
- no hidden lifetime or IRQ-context hazards,
- typed dispatch that is easy to audit in QEMU/GDB.

## Consequences
- Sleep and quantum timeouts are now both expressed through one shared timed-event mechanism.
- Scheduler no longer scans deadlines or computes nearest wakeup times.
- One-shot timer programming happens only in `kernel::time`.
- The design is better aligned with future IPC timeouts without changing the
  existing `IRQ -> mutable TrapFrame -> eret` preemptive switching model.
