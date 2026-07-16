# Scheduler, time, and blocking

The scheduler is a bounded single-core round-robin engine. Context switches are
committed by saving a borrowed `ActiveContext` into the current slot's owned
`SavedContext` and restoring the selected slot into the live return context.

## Task states

- `Free`: preallocated slot and stack available for spawn, with no saved-context
  ownership.
- `Ready`: runnable with one valid `SavedContext` and queued unless it is idle.
- `Running`: the sole committed resume target.
- `Blocked(reason)`: excluded from selection until the owning subsystem wakes
  it.
- `Zombie`: exited joinable thread retaining status until consumption.

Block reasons cover sleep, IPC, thread join, process wait, and stdin. The
scheduler stores enough typed identity to validate a wake but does not own the
external wait condition.

## Preemption and time

The architected timer is programmed to the nearest deadline. `kernel::time`
owns a preallocated event queue containing wakeups, IPC timeouts, and quantum
expiration. IRQ dispatch collects all expired events, updates scheduler state,
and programs the next deadline. No callback allocation or queue growth occurs
in the interrupt path.

Ready-queue insertion notifies the scheduler when a runnable peer appears so
idle cannot remain selected indefinitely. A quantum switch is committed only at
the frame-handoff boundary.

## Block and wake

A blocking operation joins waiter registration with scheduler blocking under a
short local IRQ critical section, closing lost-wakeup windows. The current frame
is saved before another task or idle is selected. Wake paths validate the
expected block identity, transition the task to ready, enqueue it, and request
rescheduling when needed.

IPC queues and time own timeout removal; process lifecycle owns process wait;
console owns stdin availability. The scheduler owns task state and queue order.

## Thread lifecycle

Thread slots and stacks are allocated/prepared at scheduler bootstrap. An
occupied slot owns exactly one inline `SavedContext`; runtime spawn initializes
a free slot without growing scheduler containers. `ThreadId`
contains index and generation, so stale handles fail after reuse. Joinable exits
retain one result for one joiner; detached exits reclaim automatically.

Kernel and user threads share scheduling mechanics but use distinct typed
context constructors and address-space initialization. User threads reference a
process and TTBR0 root; kernel threads run with TTBR0 cleared.

Production bootstrap preallocates the permanent idle thread and one static init
thread, which launches userspace `/init`. Dedicated QEMU kernel features select
their own finite static-task arrays without changing round-robin behavior.

## Constraints

- Single core; local IRQ exclusion is not SMP synchronization.
- Scheduler, ready-queue, deadline, mailbox timeout, and console wake
  transitions retain local IRQ exclusion. Task-only `PreemptLock` state is not
  part of scheduler handoff and its current IRQ-masking backend does not change
  scheduling semantics.
- No heap allocation or unbounded work in scheduling/timer fast paths.
- Idle is the permanent fallback and is never joinable or reclaimed.
- `SavedContext` layout is opaque to scheduler policy; Rust and architecture
  trap-frame handoff contracts change together inside the architecture facade.

Related decisions: ADR-0003, ADR-0005, ADR-0006, ADR-0011 through ADR-0014,
ADR-0020, and ADR-0027 through ADR-0029.
