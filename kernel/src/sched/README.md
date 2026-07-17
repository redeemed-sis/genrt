# Scheduler, time, and blocking

The scheduler is a bounded single-core round-robin engine. `Thread` is its only
schedulable entity. Context switches save a borrowed `ActiveContext` into the
current thread's owned `SavedContext` and restore the selected thread into the
live return context.

## Thread table and states

`ThreadId { index, generation }` directly indexes the preallocated
`ThreadTable`. Lookup is O(1) and succeeds only when the slot is occupied and
its generation matches. Release makes the old ID stale; reuse advances the
generation before publication.

A free `ThreadSlot` parks one preallocated kernel stack and retains the next
checked wait sequence. It has no `Thread` or `SavedContext`. An occupied slot
contains one `Thread`, which owns:

- `ThreadState`;
- one non-copyable `SavedContext`;
- the parked kernel stack;
- active wait metadata and join/exit state;
- optional `UserThreadResources { AddressSpaceId, OwnedUserStack }`.

The lifecycle states are:

- `Ready`: runnable and queued unless it is idle;
- `Running`: the sole committed resume target;
- `Blocked`: excluded from selection while one exact wait is active;
- `Exited`: joinable terminal state retaining exit code and resources until
  generic join/reap.

The accepted transition table is:

| Source | Destination | Operation |
| --- | --- | --- |
| free slot | `Ready` | bootstrap or runtime thread publication |
| `Ready` | `Running` | initial dispatch or thread selection |
| `Running` | `Ready` | optional round-robin switch |
| `Running` | `Blocked` | commit one prepared wait and hand off |
| `Blocked` | `Ready` | complete the exact active wait token |
| `Running` | `Exited` | joinable exit |
| `Exited` | free slot | join/reap and resource extraction |

Detached kernel-thread exit may perform the final two edges inside one
mandatory handoff. User threads are joinable because their non-copyable stack
must pass through generic reap. Internal impossible transitions panic.

All lifecycle mutations pass through the private `sched::transition` layer.
That layer changes state, generation, `current`, ready membership, and resource
publication/release as one scheduler operation. Transition selection remains
separate from architecture context handoff.

## Process boundary

The scheduler contains no process handle, process status, parent relationship,
or process-specific wait semantics. A user thread retains only the opaque,
non-owning `AddressSpaceId` needed for TTBR0 activation and owns its
`OwnedUserStack`. The process table owns the corresponding
`OwnedUserAddressSpace`, image, descriptors, cwd, relationships, terminal
status, and `main_thread: ThreadId`.

Kernel and process-backed user threads use the same exit/join/reap lifecycle.
The process layer owns a fixed reverse index from thread slot to its process
handle and validates both handle generations plus `main_thread` on lookup.
This keeps current-process resolution O(1) without storing process metadata in
scheduler state. The process layer reaps the main thread before destroying its
address space.

## Preemption and time

The architected timer is programmed to the nearest deadline. `kernel::time`
owns a preallocated event queue containing exact wait deadlines and quantum
expiration. IRQ dispatch collects expired events, completes deadline tokens,
updates scheduler state, and programs the next deadline. No callback allocation
or queue growth occurs in the interrupt path.

Ready insertion requests scheduling when a runnable peer appears, so idle
cannot remain selected indefinitely. A quantum switch is committed only at the
frame-handoff boundary. Handoff code saves/restores `SavedContext`, activates
the selected `AddressSpaceId` or clears TTBR0, and never resolves a process.

Thread preemption exclusion is nested and IRQ-enabled. Quantum expiration and
deadline wakeups continue bounded IRQ bookkeeping while a guard is active, but
optional handoff leaves the current thread `Running` and records one coalesced
request. Timer IRQ return and the private EL1 `PreemptCheckpoint` sched call
consume that request only at depth zero. Kernel yield uses the same checkpoint
and cannot bypass a guard.

## Wait registration and completion

Each blocking episode has one scheduler-owned `WaitToken` containing the full
`ThreadId` and a per-slot sequence. Active wait metadata belongs to the occupied
thread; the next sequence returns to the free slot on reap and therefore
survives generation reuse.

A blocking operation prepares wait metadata, publishes the exact token with
its condition owner, releases that owner, and commits the prepared wait.
Completion before commit records the cause and leaves the caller running;
completion after commit performs `Blocked -> Ready`. The first exact
completion wins. Duplicate and stale generation/sequence completions cannot
change cause or ready membership.

Mailbox queues, thread/process lifecycle, console, and time retain their own
payloads and cleanup policy. Scheduler wait metadata contains only exact token,
phase, and cause. No owner lock crosses scheduler commit/completion, and the
scheduler never calls back into an owner while changing lifecycle state.

Sleep, blocking IPC/stdin/join/process waits, and terminal transitions fail
fast if thread preemption is disabled. They do not defer ownership publication
across an active guard.

## Thread lifecycle

Scheduler bootstrap allocates all slots, kernel stacks, and container capacity.
Runtime spawn moves a parked stack into a new `Thread`, initializes its inline
context, and queues it without growing scheduler storage. Reap extracts any
`OwnedUserStack` under short local-IRQ exclusion and destroys it only after the
guard is released.

Production bootstrap publishes the permanent idle thread and one static init
thread, which launches userspace `/init`. Dedicated QEMU features select their
own finite static-thread arrays without changing round-robin behavior.

## Constraints

- Single core; local IRQ exclusion is not SMP synchronization.
- No heap allocation or unbounded work in scheduling, timer, or frame-handoff
  fast paths.
- Idle is permanent, non-joinable, and never reclaimed.
- Debug and QEMU-test builds run a bounded, allocation-free invariant walk.
- `SavedContext` layout is opaque to scheduler policy; Rust and architecture
  trap-frame contracts change together inside the architecture facade.
- The current process model has one user thread. Multiple threads sharing one
  process/address space remain future work.

Related decisions: ADR-0003, ADR-0005, ADR-0006, ADR-0011 through ADR-0014,
ADR-0020, and ADR-0027 through ADR-0033.
