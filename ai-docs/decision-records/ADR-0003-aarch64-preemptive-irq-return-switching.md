# ADR-0003: AArch64 Preemptive IRQ-Return Context Switching

## Status
Accepted

## Context
`genrt` already had EL1 vectors, GICv2 timer IRQ delivery, and fixed-priority scheduling.
The prior switching direction did not perform the handoff on the IRQ return path and could allow scheduler bookkeeping to diverge from actual resumed CPU context.

## Decision
- Introduce a full AArch64 `TrapFrame` (`x0..x30`, `sp`, `elr`, `spsr`) for IRQ save/restore.
- Rework EL1 IRQ entry/exit so the active return frame is mutable during timer IRQ handling.
- Commit switch by frame replacement during IRQ handling:
  - save interrupted frame into current task storage,
  - copy selected task frame into active IRQ frame,
  - commit scheduler running-state transition in the same handoff path.
- Bootstrap never-run tasks from an initial trap frame and enter first task via restore + `eret`, matching normal resume semantics.

## Rationale
Context switch as exception-return frame replacement is the minimal deterministic preemptive mechanism for EL1-only kernel threads.
It keeps architecture-specific register semantics in `arch/aarch64` and keeps scheduling policy in `kernel::sched`.

## Consequences
- Timer IRQ can preempt and switch tasks without cooperative schedule points.
- Each runnable task owns a saved full resume frame.
- Scheduler `Running` state is updated only when frame handoff is committed.
- Design is ready for later extensions (sleep/wakeup, IPC) while remaining single-core and EL1-only.
