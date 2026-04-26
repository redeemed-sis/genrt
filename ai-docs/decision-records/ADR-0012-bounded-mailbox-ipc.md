# ADR-0012: Bounded Mailbox IPC for Kernel Tasks

## Status
Accepted

## Context

`genrt` already has the scheduler and time foundations needed for a first IPC
primitive:

* IRQ-return-based preemptive task switching
* explicit `Ready` / `Running` / `Blocked` task states
* scheduler-owned block/wake semantics
* time-owned one-shot deadline events
* a fixed bootstrap heap with an explicit allocation policy

The next milestone needs inter-task communication without weakening the current
hard real-time constraints. In particular, mailbox send/recv paths must not grow
heap-backed structures while tasks are being blocked or woken.

## Decision

Add a minimal bounded mailbox IPC primitive for kernel tasks.

Key points:

* message type is selected by the mailbox client through `Mailbox<T>`
* each mailbox owns a heap-backed ring buffer allocated to its fixed capacity
  at creation time
* send and recv wait queues are `VecDeque<TaskId>` values reserved at mailbox
  creation time
* `try_send` and `try_recv` are O(1) and non-blocking
* blocking `send` and `recv` retry after wakeup
* mailbox state is protected by the shared kernel IRQ-save lock abstraction
  (`kernel::sync::IrqSpinLock`)
* wait queue insertion and scheduler blocking are joined through a controlled
  typed task-call path so a task cannot be woken between "became a waiter" and
  "became Blocked"
* waking a mailbox waiter uses the existing scheduler `wake_task` path and does
  not allocate
* the first milestone keeps one bootstrap-created demo mailbox in the demo task
  module instead of introducing a dynamic mailbox registry

Timeouts are intentionally not part of this ADR. The wait queues and retry model
are shaped so `send_timeout` and `recv_timeout` can later be layered on top of
the existing time-owned deadline queue.

The architecture boundary is intentionally typed rather than callback-based:
AArch64 exposes a single synchronous `arch_task_call(request_ptr)` entry, and
the kernel dispatches known request structures. This avoids adding a new
architecture function for each blocking primitive while still keeping arbitrary
callbacks and hidden lifetimes out of exception context. Passing a request
pointer also keeps the architecture ABI stable if later task calls need more
arguments.

## Consequences

Positive:

* producer and consumer kernel tasks can exchange messages through a bounded
  queue
* receivers block on empty mailboxes and senders block on full mailboxes
* blocked mailbox waiters do not participate in scheduling until woken
* mailbox fast paths remain allocation-free after bootstrap
* the scheduler/time ownership split remains unchanged
* the lock call sites are ready for a future SMP implementation to add a spin
  acquisition behind the same IRQ-save API

Limitations:

* single-core only
* kernel tasks only
* one demo mailbox, no registry yet
* no timeout events yet
* no priority inheritance or SMP synchronization
