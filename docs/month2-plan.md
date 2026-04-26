# Month 2 plan

Month 2 should **stabilize and operationalize** the execution model that month 1 introduced.

The project already has the hardest conceptual milestone behind it:

- timer IRQs work
- tasks have saved execution state
- IRQ-return-based switching exists
- demo tasks run

Month 2 should therefore avoid jumping immediately into MMU, user mode, or second-architecture work.

Instead, it should make the current kernel core trustworthy.

---

## Month-2 goals

By the end of month 2, genrt should have:

- a hardened single-core preemptive scheduler core
- explicit block/wakeup semantics
- timer-driven sleeping
- one bounded IPC primitive
- better tracing for frequent events
- a more reproducible debug/regression loop

---

## Non-goals for month 2

These are important, but should stay out of month 2:

- MMU and address spaces
- EL0/user mode
- SMP scheduling
- second architecture bring-up
- full driver model
- dynamic memory allocator as a primary milestone

The project will move faster if month 2 finishes the kernel core semantics first.

---

## Week 5 — preemption and scheduler hardening

### Objectives

Turn the current preemptive scheduler from “working prototype” into a cleaner, more defensible kernel core.

### Work items

- remove or isolate mutable scheduler access from normal preemptible task context
- avoid runtime scheduler mutation from demo tasks unless it is explicitly protected and designed
- tighten task-state semantics:
  - define what `Running`, `Ready`, and `Blocked` mean
  - ensure bookkeeping always matches the actual IRQ-return target
- document and assert trap-frame layout invariants
- document the `boot.s` save/restore contract more clearly
- reduce ambiguous bootstrap/demo-only behavior in the scheduler API

### Definition of done

- the scheduler has one clear ownership model
- no obvious preemption-time aliasing hazards remain in normal task/demo paths
- task-state transitions are explicit and auditable
- the system can run in QEMU for an extended interval without corrupting scheduler state

---

## Week 6 — sleep/wakeup on top of the timer tick

### Objectives

Make the timer useful for more than round-robin preemption.

### Work items

- add a minimal sleep API such as:
  - `sleep_ticks(n)`
  - or `sleep_until(deadline)`
- introduce blocked-task wakeup via the periodic tick
- ensure idle runs only when there is no runnable work
- keep the first version simple:
  - static task set
  - no heap
  - bounded scans are acceptable

### Suggested design

For the first version, an O(N) scan over the static task table is acceptable and preferable to a more complicated timer structure.

### Definition of done

- a task can sleep for a number of ticks
- sleeping tasks leave the runnable set
- the timer wakeup path restores them to runnable state
- demo output clearly shows tasks sleeping and waking without busy-waiting

---

## Week 7 — bounded mailbox / queue IPC

### Objectives

Add the first useful inter-task communication primitive.

### Work items

- implement one bounded mailbox or ring-buffer queue
- no heap allocation
- explicit capacity
- send/recv operations
- block on empty/full if needed
- optional timeout integration if the sleep/wakeup path is already stable

### Suggested scope

Keep the first IPC primitive narrow:

- kernel threads only
- single-core only
- one queue type
- deterministic bounded operations

### Definition of done

- two tasks can exchange messages through the mailbox
- queue state remains bounded and deterministic
- send/recv can interact correctly with block/wakeup
- a simple ping-pong demo works under QEMU

### Current implementation note

The first bounded mailbox milestone is now implemented for EL1 kernel tasks:

- client-defined message type through `Mailbox<T>`
- heap-preallocated fixed-capacity ring buffer
- preallocated bounded send and recv wait queues
- non-blocking `try_send` / `try_recv`
- blocking `send` / `recv` through scheduler block/wake
- one bootstrap-created demo mailbox owned by the demo task module

Timeout variants are intentionally left for the follow-up milestone and should
be built on top of the existing time-owned deadline queue.

---

## Week 8 — tracing, regression loop, and documentation cleanup

### Objectives

Make the project easier to debug and safer to change.

### Work items

- add a lightweight in-memory trace ring buffer for high-frequency events
- keep UART logging for low-rate human-readable output
- add QEMU smoke-test workflow in `xtask` where practical
- improve panic and exception reporting consistency
- refresh docs and ADRs to match the actual codebase
- remove stale “phase0/week2 only” assumptions from helper docs and commands

### Suggested trace events

Useful early events include:

- tick
- switch
- block
- wake
- mailbox send/recv
- fatal exception entry

### Definition of done

- high-frequency scheduling events no longer require direct UART spam
- there is a basic reproducible smoke-test loop
- the docs describe the actual system, not the old bring-up snapshot

---

## Recommended milestone order

The best month-2 sequence is:

1. **scheduler hardening**
2. **sleep/wakeup**
3. **bounded IPC**
4. **trace buffer + regression loop**

That order matters.

### Why this order is best

#### 1. Hardening before expansion
The current preemptive core is the foundation. If ownership and state transitions are still fuzzy, every new subsystem will amplify that instability.

#### 2. Sleep/wakeup before IPC
Once tasks can block and wake on time, the scheduler becomes much more realistic. IPC built on top of that model will be far cleaner.

#### 3. IPC before MMU
A mailbox exercises real kernel semantics immediately:
- blocking
- waking
- fairness
- timeout behavior

MMU work is important, but it adds complexity before the current kernel-thread model is fully exercised.

#### 4. Trace buffering after concurrency increases
Once more scheduler and IPC events exist, direct UART tracing becomes too expensive. A ring buffer becomes valuable at exactly that point.

---

## End-of-month success criteria

Month 2 is successful if genrt can demonstrate all of the following on AArch64/QEMU:

- periodic preemptive scheduling remains stable
- tasks can sleep and wake on timer deadlines
- at least one bounded mailbox/queue works
- switch/block/wake behavior can be observed without excessive UART overhead
- documentation matches the codebase

---

## What should come after month 2

Only after month 2 should the project seriously move to:

- MMU and memory management
- privilege separation / EL0
- second architecture bring-up
- broader driver work

That sequence keeps genrt grounded in a stable kernel core rather than splitting effort too early.
