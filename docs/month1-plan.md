
## `docs/month1-plan.md`

```md
# Month 1: plan closure and actual outcome

This document updates the original month-1 plan to reflect what actually landed in the repository.

---

## Original month-1 intent

The original month-1 goal was to move from repository bootstrap to the first usable kernel execution skeleton:

- repository and workflow bootstrap
- AArch64 boot entry
- serial output
- timer interrupt path
- minimal scheduler structure
- first trace/debug points

That intent was achieved, but the implementation evolved in a different direction than the original Week 4 wording suggested.

---

## What was completed

### Week 1 — repository bootstrap

Completed:

- Rust workspace layout
- pinned toolchain
- `xtask` workflow
- `justfile`
- `AGENTS.md`
- initial AI/ADR documentation structure
- `bootinfo` crate

### Week 2 — AArch64 boot bring-up

Completed:

- `_start` in `boot.s`
- `.bss` clearing
- EL1 vector installation through `VBAR_EL1`
- boot stack setup
- Rust handoff through `rust_entry`
- early PL011 console output
- `BootInfo` initialization and handoff into `kernel_main`

### Week 3 — interrupt/timer path

Completed:

- GICv2 initialization for QEMU `virt`
- architected physical timer programming
- IRQ acknowledgment and EOI path
- periodic timer rearm
- kernel tick accounting

This moved the system from “first boot” to “hardware interrupt-driven kernel loop”.

### Week 4 — scheduler/execution model

The original wording for Week 4 was:

- fixed-priority scheduler skeleton
- bounded queue or mailbox
- first trace points

What actually landed was different:

#### Completed
- scheduler module
- static per-task stacks
- task bootstrap through prepared saved state
- full trap-frame-based IRQ handling
- IRQ-return-based preemptive context switching
- round-robin runnable-task rotation
- basic demo tasks
- minimal formatted logging and trace levels
- improved fatal exception diagnostics

#### Not completed in month 1
- bounded mailbox / queue IPC
- sleep/wakeup blocking semantics
- timeout-based waiting
- buffered tracing

---

## What changed versus the original plan

### 1. The scheduler direction changed

The original plan expected a **fixed-priority scheduler skeleton** first.

The actual implementation moved to a **preemptive round-robin kernel-thread model** instead.

This was a reasonable change because it solved the harder problem first:

> how tasks really start, stop, resume, and switch at IRQ return boundaries

That was more foundational than preserving the original fixed-priority policy wording.

### 2. Observability became more important than originally expected

As soon as the project gained:

- real timer IRQs
- saved task frames
- preemption
- fatal exception reporting

the old `puts()`-style debugging became too weak.

As a result, month 1 also gained:

- allocation-free formatted output
- log levels
- trace-style debug points
- better exception dumps

### 3. IPC slipped out of month 1

This was the right tradeoff.

Bounded IPC would have been much less useful before the project had:

- real task switching
- saved execution frames
- periodic preemption
- a clearer kernel execution model

So IPC moved out, while the execution model moved in.

---

## Month-1 result

At the end of month 1, genrt is no longer just a scaffold or a “first boot” demo.

The repository now has:

- real EL1 boot bring-up
- working interrupt delivery
- periodic kernel ticks
- a global timebase
- static kernel threads
- IRQ-return-based preemption
- minimal formatted logging

That makes month 1 a success.

---

## What remains unresolved entering month 2

The new execution model is real, but it still needs hardening.

The main carry-over items are:

- scheduler ownership and mutation discipline need tightening in a preemptive world
- task state semantics should become more explicit
- sleep/wakeup is still missing
- IPC is still missing
- direct UART trace is useful for bring-up but too expensive for sustained tracing
- platform/arch separation is still not clean enough
- `BootInfo` is still much thinner in practice than in type shape

---

## Bottom line

Month 1 achieved the most important kernel bring-up transition:

> from early boot + IRQ scaffolding to a real preemptive execution model on AArch64/QEMU

That is the correct point to treat as the end of month 1 and the start of month 2 hardening work.
