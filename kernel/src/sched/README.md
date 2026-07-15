# Scheduler, time, and blocking

The scheduler is a bounded single-core round-robin engine. Context switches are
committed by selecting the trap frame restored on IRQ or controlled synchronous
return.

## Task states

- `Free`: preallocated slot, frame, and stack available for spawn.
- `Ready`: runnable with a valid saved frame and queued unless it is idle.
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

Thread slots and stacks are allocated/prepared at scheduler bootstrap. Runtime
spawn initializes a free slot without growing scheduler containers. `ThreadId`
contains index and generation, so stale handles fail after reuse. Joinable exits
retain one result for one joiner; detached exits reclaim automatically.

Kernel and user threads share scheduling mechanics but have distinct frame and
address-space initialization. User threads reference a process and TTBR0 root;
kernel threads run with TTBR0 cleared.

Production bootstrap preallocates the permanent idle thread and one static init
thread, which launches userspace `/init`. Dedicated QEMU kernel features select
their own finite static-task arrays without changing round-robin behavior.

## Constraints

- Single core; local IRQ exclusion is not SMP synchronization.
- No heap allocation or unbounded work in scheduling/timer fast paths.
- Idle is the permanent fallback and is never joinable or reclaimed.
- Rust and architecture trap-frame handoff contracts change together.

Related decisions: ADR-0003, ADR-0005, ADR-0006, ADR-0011 through ADR-0014,
and ADR-0020.
