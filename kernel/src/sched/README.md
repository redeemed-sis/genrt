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

All lifecycle mutations pass through the private `sched::transition` layer.
That layer changes task state, generation, `current`, and ready-queue membership
as one scheduler operation. Other scheduler modules own policy and context
handoff, but cannot write those fields directly.

The accepted transition table is:

| Source | Destination | Operation |
| --- | --- | --- |
| `Free` | `Ready` | bootstrap or runtime publication |
| `Ready` | `Running` | initial dispatch or task selection |
| `Running` | `Ready` | optional round-robin switch |
| `Running` | `Blocked(reason)` | mandatory blocking handoff |
| `Blocked(reason)` | `Ready` | validated wake by the owning subsystem |
| `Running` | `Zombie` | joinable exit |
| `Zombie` | `Free` | status consumption and slot reclaim |

Detached exit may perform `Running -> Zombie -> Free` inside one mandatory
handoff. Internal impossible transitions panic. Duplicate or stale
generation-bearing wake notifications remain no-ops. Ready-queue identities
are complete `ThreadId` values, so a reused slot cannot inherit stale queue
ownership. Sleep, quantum, and IPC-timeout events also carry `ThreadId`; an
expired event from an older generation cannot affect a reused slot.

## Preemption and time

The architected timer is programmed to the nearest deadline. `kernel::time`
owns a preallocated event queue containing wakeups, IPC timeouts, and quantum
expiration. IRQ dispatch collects all expired events, updates scheduler state,
and programs the next deadline. No callback allocation or queue growth occurs
in the interrupt path.

Ready-queue insertion notifies the scheduler when a runnable peer appears so
idle cannot remain selected indefinitely. A quantum switch is committed only at
the frame-handoff boundary. The transition layer returns a context-free switch
outcome; `preempt` and `thread` code then save or restore `SavedContext` values
and activate TTBR0 without reopening lifecycle state.

Task-only preemption exclusion is nested and IRQ-enabled. Quantum expiration
and deadline wakeups continue their bounded IRQ bookkeeping while a guard is
active, but optional handoff leaves the current task `Running` and records one
coalesced pending request. Timer IRQ return and the private EL1
`PreemptCheckpoint` task call consume that request only at depth zero. Kernel
yield uses the same checkpoint and therefore cannot bypass a guard.

## Block and wake

A blocking operation joins waiter registration with scheduler blocking under a
short local IRQ critical section, closing lost-wakeup windows. The transition
commits the outgoing block and incoming selection before handoff code saves the
outgoing frame and restores the selected task. Wake paths validate the expected
block identity, transition the task to ready, enqueue it, and request
rescheduling when needed.

Sleep, blocking IPC/stdin/join/process waits, and thread/process exit fail fast
if task preemption is disabled. They do not defer ownership publication across
an active guard.

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
  transitions retain local IRQ exclusion. Task-only `PreemptLock` state uses a
  nested disable depth and deferred scheduler checkpoint while allowing IRQ
  progress.
- No heap allocation or unbounded work in scheduling/timer fast paths.
- Idle is the permanent fallback and is never joinable or reclaimed.
- Debug and QEMU-test builds validate state/context/queue/current consistency
  after transitions using a bounded, allocation-free table walk.
- `SavedContext` layout is opaque to scheduler policy; Rust and architecture
  trap-frame handoff contracts change together inside the architecture facade.

Related decisions: ADR-0003, ADR-0005, ADR-0006, ADR-0011 through ADR-0014,
ADR-0020, and ADR-0027 through ADR-0031.
