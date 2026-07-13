# ADR-0013: Mailbox Timeout Semantics

## Status

Accepted

## Context

The bounded mailbox IPC primitive already owns its fixed-capacity message buffer
and send/recv wait queues. The time subsystem owns one-shot deadlines and typed
timed events. Blocking mailbox waits now need bounded timeout semantics without
adding timer callbacks, heap allocation in IRQ context, or a generic wait
framework.

## Decision

Add timeout-aware mailbox operations on top of the existing ownership split:

* `kernel::time` owns the deadline entry as `TimedEvent::IpcTimeout(TaskId)`
* `kernel::sched` stores the blocked task's IPC wait state as an opaque
  `IpcWaitToken` plus an optional timeout event handle
* `kernel::ipc` remains the sole owner of concrete IPC wait queues and removes a
  timed-out task from the appropriate queue by dispatching that token

The timeout event handle is the typed event value itself. A task can only be
blocked on one IPC operation at a time, so `IpcTimeout(task)` is unique while the
task is waiting. Normal IPC wakeup cancels that event before moving the task to
`Ready`; timeout dispatch verifies the scheduler wait reason, asks IPC to remove
the waiter, records `TimedOut`, and wakes the task.

Stale timeout events are ignored when the scheduler no longer shows the task as
blocked on the matching IPC wait. Missing concrete wait queue entries during a
live IPC timeout are treated as an invariant violation.

The mailbox object must remain alive and unmoved while tasks may be waiting on
it, because the IPC wait token contains the mailbox control pointer for timeout
cleanup. This is the same lifetime rule already required by blocking mailbox
waits.

## Consequences

Positive:

* timeout-aware send/recv operations can return an explicit timeout result
* normal wakeup cancels pending timeout events
* timeout wakeup removes the task from the owning IPC wait queue before waking it
* time remains callback-free and typed-event based
* the scheduler knows only about generic IPC waits, not specific IPC primitives
* IRQ/time/scheduler fast paths continue to operate on preallocated storage

Limitations:

* single-core only
* one IPC timeout event per task
* no dynamic mailbox registry
* no generic wait queue framework
