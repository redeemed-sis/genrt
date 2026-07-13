# ADR-0005: One-Shot Timer Deadline Engine

## Status

Accepted

## Context
`genrt` initially used a periodic timer heartbeat as both:
- scheduler quantum source,
- and sleep/wakeup source.

That was simple for bring-up, but it tied all timed behavior to a coarse tick and
made future timeout-based mechanisms depend on ad-hoc work performed on every IRQ.

## Decision
- Use the architected timer monotonic counter as the kernel time domain.
- Represent timed state as absolute deadlines in counter units:
  - per-task `sleep_deadline`,
  - scheduler `quantum_deadline` for the current running task.
- Program a single hardware one-shot timer to the nearest deadline in the system.
- On timer IRQ:
  - read the current counter,
  - wake all expired sleeping tasks,
  - handle scheduler quantum expiry,
  - possibly replace the active IRQ-return frame,
  - compute the next nearest deadline,
  - re-arm the timer once before returning from IRQ.
- Expose sleep in real time units rather than through a synthetic periodic tick API.

## Rationale
This keeps the timed model explicit and composable:
- one monotonic timebase,
- one hardware timer,
- one deadline-selection path.

It also creates a direct foundation for future timeout-based IPC without
re-introducing a mandatory periodic heartbeat.

## Consequences
- Sleep and quantum handling now share one timed mechanism.
- Timer interrupts happen only when the earliest deadline expires.
- The first implementation remains simple and deterministic via O(N) scans over
  the static task table.
- `IRQ -> mutable TrapFrame -> eret` preemptive switching remains unchanged.
