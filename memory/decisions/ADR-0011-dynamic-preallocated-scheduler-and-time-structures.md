# ADR-0011: Dynamic Preallocated Scheduler and Time Structures

## Status

Accepted

## Subsequent refinement

[ADR-0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md) replaces
only this decision's boxed saved-frame backing with inline `SavedContext`
ownership inside the already preallocated `Vec<Task>`. Boxed task stacks,
bootstrap reservation, bounded capacities, and allocation-free handoff remain
unchanged.

## Context

`genrt` had already separated scheduler and time ownership correctly:

* `kernel::sched` owned task states, block/wake semantics, and frame handoff
* `kernel::time` owned timed events, nearest-deadline selection, and timer rearming

However, both subsystems still relied on static fixed-size arrays for their
main storage:

* static task table
* static task stacks and saved frames
* static timed-event slots

That shape was becoming a practical limit for future IPC, wait queues, and
timeout work.

At the same time, the kernel had already established an explicit allocation
contract:

* heap allocation is allowed during bootstrap and in ordinary task context
* heap allocation is not a supported design target in IRQ, scheduler handoff,
  or time fast paths

## Decision

The scheduler and time subsystem now use heap-backed dynamic structures that
are fully allocated and reserved during bootstrap.

Key points:

* `TaskId` is now an explicit type shared between scheduler and time
* scheduler task storage is a heap-backed `Vec<Task>`
* each task owns stable boxed storage for:
  * task stack
  * saved frame
* scheduler runnable order is represented by a preallocated `VecDeque<TaskId>`
* timed events are stored in a heap-backed preallocated deadline queue owned by
  `kernel::time`
* the deadline queue supports schedule, cancel, and deadline update without
  allocating in IRQ paths
* timer IRQ handling and scheduler handoff continue to operate only on
  preallocated bounded storage

## Consequences

Positive:

* removes the main static bring-up limitation from scheduler/time state
* keeps the existing ownership split intact
* preserves the IRQ-return execution model
* prepares the kernel for IPC timeouts and other bounded dynamic kernel
  structures

Limitations:

* no runtime task creation or deletion yet
* dynamic container capacity is fixed once bootstrap finishes
* exceeding reserved scheduler/time capacity is treated as a kernel bug and
  results in panic instead of implicit growth in IRQ-critical paths
