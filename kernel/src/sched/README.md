# Scheduler, time, and blocking

The scheduler is a bounded round-robin engine with one shared thread table and
fixed per-logical-CPU contexts. CPU0 preallocates one permanent idle slot for
every expected CPU. Each registered CPU initializes local idle/current state
and becomes online only at first saved-frame entry. `Thread` is the only
schedulable entity. Context
switches save a borrowed `ActiveContext` into the current thread's owned
`SavedContext` and restore the selected thread into the live return context.

`CpuId` is a checked logical identity supplied by the boot CPU registry; generic
scheduler code never uses raw MPIDR. Every context owns its current thread,
idle thread, ready queue, and initialized/online state. Preemption bookkeeping
uses separate fixed CPU-local storage available before heap initialization and
retained after scheduler publication. CPU0 and every DTB-described secondary
are registered before scheduler bootstrap; CPU0 reserves idle capacity for the
full topology, and each CPU initializes one local context. `ThreadAttrs` chooses an immutable home
CPU before publication. Scheduler storage and all per-CPU time queues are
published by CPU0 before secondary startup. Architecture entry code owns each
CPU's local interrupt readiness.
Local wakeups enter that CPU's ready queue. Remote
wakeups publish into a bounded per-target ingress that only the target CPU
drains into its local ready queue after a targeted scheduler SGI. One protected
per-target bit coalesces notifications without replacing ingress as the source
of truth. A sole current or idle thread has no polling quantum; RR deadlines are
armed only while a local runnable peer exists. Independent single-threaded
processes may run on secondary CPUs, but there is no migration, work stealing,
remote timer insertion, or address-space sharing across CPUs.

Current-CPU entry points resolve `CpuId` once and bind a short-lived
`CpuScheduler` view. Local transition, wait, and preemption methods use the
view's stable identity instead of repeating architecture lookup or accepting a
`CpuId` parameter at every layer. Bootstrap, explicit affinity, wakeup to a
thread's `home_cpu`, and invariant validation select a target CPU explicitly.

The bounded `ThreadTable`, free-slot lifecycle storage, and remote ingress use a
cross-CPU IRQ-safe scheduler lock so publication and lifecycle transitions stay
atomic. CPU contexts remain owner-local: mutable `current`, `idle`, ready-queue,
and online-state access is rejected from another CPU. Fixed preemption storage
performs the same current-CPU ownership check independently. Explicit affinity
to an unregistered or offline CPU is rejected. Remote ingress publication
requests an architecture scheduler notification but does not program a remote
timer or mutate remote preemption state directly.

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

For a user thread, `AddressSpaceId::owner_cpu()` must equal immutable
`home_cpu`. Scheduler publication checks this before runnable visibility;
scheduler handoff and the VM facade reject activation from another CPU.

## Preemption and time

Each logical CPU owns one preallocated deadline queue and its architected
physical timer. For secondaries, CPU-local queue publication precedes PSCI
startup, timer PPI enable, and IRQ unmasking. CPU0 initializes its local timer
hardware earlier with IRQ masked and does not unmask until generic queue and
scheduler publication are complete. Timed events retain that CPU identity, and only the owner CPU
inserts events or programs its one-shot nearest deadline. IRQ dispatch collects
expired events, releases the queue lock, then directly invokes the narrow
scheduler facade to complete tokens, account quantum expiration, and perform
IRQ-return handoff. No callback table, allocation, or queue growth occurs in
the interrupt path.

Cross-CPU cancellation may remove an exact event without touching the remote
physical timer; at worst the old deadline causes one harmless early interrupt.
Prompt remote insertion remains rejected because the scheduler SGI is a
payload-free runnable-work notification, not a remote timer command channel.

Ready insertion requests scheduling when a runnable peer appears, so idle
cannot remain selected indefinitely. Remote publication and its coalescing bit
change under shared scheduler ownership before a targeted SGI is issued. The
target IRQ drains all ingress, clears the bit in that same ownership domain,
and requests its normal local checkpoint; duplicate SGIs are harmless. A
quantum or IPI switch is committed only at the frame-handoff boundary. Handoff
code saves/restores `SavedContext`, activates the selected `AddressSpaceId` or
clears TTBR0, and never resolves a process.

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

Exit first records the outgoing thread in one CPU-local retired slot. The
thread remains occupied while exception return still uses its kernel stack;
only the next scheduler entry on that CPU may finalize join completion and
publish the stack for reuse. This prevents another CPU from reusing a live
handoff stack. Exit requests a coalesced local scheduler SGI so that this next
entry occurs on the replacement stack even when no timer deadline is pending.

Production bootstrap publishes the permanent idle thread and one static init
thread, which launches userspace `/init`. Dedicated QEMU features select their
own finite static-thread arrays without changing round-robin behavior.

## Constraints

- Device IRQs remain CPU0-only. Registered secondary CPUs may run permanent
  idle threads, explicitly pinned kernel threads, and independently owned user
  processes.
- Local IRQ exclusion is not SMP synchronization; shared owners use explicit
  cross-CPU locks.
- No heap allocation or unbounded work in scheduling, timer, or frame-handoff
  fast paths.
- Idle is permanent, non-joinable, and never reclaimed.
- Debug and QEMU-test builds run a bounded, allocation-free invariant walk.
- `SavedContext` layout is opaque to scheduler policy; Rust and architecture
  trap-frame contracts change together inside the architecture facade.
- The current process model has one user thread. Multiple threads sharing one
  process/address space remain future work.

Related decisions: ADR-0003, ADR-0005, ADR-0006, ADR-0011 through ADR-0014,
ADR-0020, ADR-0027 through ADR-0033, and ADR-0035 through ADR-0041.
