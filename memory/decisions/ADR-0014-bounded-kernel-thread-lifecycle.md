# ADR-0014: Bounded Kernel Thread Lifecycle

## Status

Accepted

## Context

The scheduler started as a bootstrap task runner with explicit ready/running/
blocked states. The next milestone needs runtime kernel thread creation and
completion without changing the current single-core, IRQ-return trap-frame
handoff model or weakening the no-allocation IRQ fast-path rule.

## Decision

Add a bounded kernel-thread lifecycle API:

* `thread_spawn(entry, ThreadArg, attrs)`
* `thread_exit(code)`
* `thread_join(id)`

Public thread handles are `ThreadId { index, generation }`. The index selects a
scheduler slot, and the generation is bumped before a free slot is reused. All
join/lifecycle lookups validate both fields and reject free slots, so stale
handles cannot name a later thread that reused the same index.

The scheduler owns a fixed pool of thread slots prepared during bootstrap. Each
slot owns stable stack storage and saved trap-frame storage; runtime spawn only
selects a free slot, updates its generation, initializes the frame, and queues
the thread. The first stack class is fixed at 8 KiB per thread slot. Variable
stack classes and guard pages are left for later MMU work.

Bootstrap and runtime-spawned thread entries both use `fn(ThreadArg) -> usize`.
`ThreadArg` is a transparent payload for a small integer or explicit raw pointer
context. The initial frame enters a kernel trampoline that calls the entry
function and routes its return value into `thread_exit(code)`. Explicit exit and
returned entry functions both use the controlled task-call/SVC path: the
scheduler marks the current thread exited, wakes a joiner if present, copies the
next runnable thread frame into the active trap frame, and never returns to the
exited thread.

The implementation is split into scheduler submodules: bootstrap owns startup
slot preparation, sleep owns sleep task-calls, preempt owns task state and
IRQ-return handoff, and thread owns the dynamic lifecycle API.

Joinable threads become zombies until joined. A successful join returns the exit
code and reclaims the target slot for future reuse. Detached threads are
reclaimed on exit. Only one joiner is supported; a second concurrent join returns
`JoinInProgress`. The idle thread is special: it is created at bootstrap, is not
joinable, cannot exit, and is never reclaimed.

## Consequences

Positive:

* runtime kernel thread creation is bounded and does not grow scheduler
  containers
* thread return values are observable through join
* thread slots can be reused without stale-handle aliasing
* the existing trap-frame handoff remains the only context-switch mechanism
* IRQ/time/scheduler fast paths continue to operate on preallocated storage

Limitations:

* single-core only
* one fixed stack class
* one joiner per joinable thread
* no cancellation, forced kill, TLS, priorities, or userspace process model yet
* threads are responsible for releasing external resources before exit
