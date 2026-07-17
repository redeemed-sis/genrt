# ADR-0033: Unified thread model and scheduler/process separation

## Status

Accepted

## Context

The scheduler exposed `ThreadId` as its public lifecycle handle while retaining
a second internal `Task`/`TaskId` model. User scheduler entries also carried a
`ProcessId`, and the process table owned the main thread's user stack. These
overlapping identities and ownership domains made slot lookup, process cleanup,
and the lifetime of TTBR0-dependent resources harder to audit.

The typed active/saved context, centralized transition, deferred reschedule,
and exact wait-registration decisions already provide the required lifecycle
boundaries. The remaining ambiguity is structural: one execution entity should
own execution resources, while the process layer should own process-wide state.

## Decision

- `Thread` is the only schedulable entity. The scheduler no longer has `Task`,
  `TaskId`, `TaskState`, `TaskSlot`, `TaskTable`, or `StaticTask` types.
- `ThreadId { index, generation }` directly indexes one bounded `ThreadSlot`.
  Lookup is O(1), validates both fields, and rejects free slots. Generation is
  advanced before a released slot is reused.
- A free `ThreadSlot` parks its preallocated kernel stack and retains the next
  checked wait sequence. An occupied slot contains one `Thread`; that `Thread`
  owns its `ThreadState`, `SavedContext`, kernel stack, active wait metadata,
  join/exit metadata, and optional user resources.
- `UserThreadResources` contains a non-owning, copyable `AddressSpaceId` and one
  non-copyable `OwnedUserStack`. The user stack owns its mapped virtual range
  metadata and physical frames. It remains with an exited thread until generic
  join/reap extracts it.
- `Process` exclusively owns `OwnedUserAddressSpace`, loaded ELF mappings, file
  descriptors, cwd, parent/child relations, process status, process waiters,
  and `main_thread: ThreadId`. It does not own execution contexts or stacks.
- `AddressSpaceId` is opaque scheduler activation metadata, not an owning
  capability. The process-owned address space must outlive every thread that
  retains its ID.
- Scheduler code contains no `ProcessId`, process lookup, parent relationship,
  process status, or process-specific wait kind. The process layer finds the
  current process in O(1) through a fixed reverse index from thread slot to
  `ProcessId`. Lookup validates both generations and the process-owned
  `main_thread`; the index never enters scheduler state.
- Kernel and process-backed user threads use one join lifecycle:

  ```text
  Running -> Exited(exit code) -> joined/reaped -> free slot
  ```

  The process layer independently stores `ProcessExitStatus`. `process_join`
  and `waitpid` consume process-owned status, then use ordinary thread join/reap
  for `main_thread` before destroying process resources.
- Process cleanup follows this order:

  ```text
  terminal process status
    -> generic main-thread join/reap
    -> release OwnedUserStack
    -> release ELF segment frames
    -> destroy OwnedUserAddressSpace
  ```

  Owning resources are extracted under short local-IRQ exclusion and destroyed
  afterward.
- First-process spawn and `fork` transfer a prepared `OwnedUserStack` into a
  joinable user thread exactly once. Failure keeps every owner in rollback
  state. `execve` stages a new address space, image, and stack; commit activates
  and swaps thread/process ownership before replacing the live user context,
  then destroys old resources outside exclusion.
- The private controlled EL1 entry lives in `sched::call`. Its SVC immediate and
  numeric operation values are unchanged; the former process-join operation
  number remains reserved. EL0 syscall ABI is unchanged.
- This issue adds pure `ThreadTable` unit coverage only. Existing
  `kernel-contract`, `userspace-contract`, `shell-contract`, and `user-fault`
  suites provide integration regression coverage without new scenarios or
  protocol markers.

## Invariants

- A live `ThreadId` names exactly one occupied slot generation; a stale ID never
  names a reused thread.
- Free slots contain no `Thread`, `SavedContext`, active wait, join result, or
  user resource. Occupied threads own their parked kernel stack.
- The per-slot wait sequence survives release and reuse, while active wait state
  belongs only to the occupied thread.
- A kernel thread has no user resources. A user thread owns exactly one
  `OwnedUserStack` and one non-owning `AddressSpaceId`.
- Scheduler code neither stores nor resolves `ProcessId`.
- The process-owned reverse index and `main_thread` are published together.
  The index entry is cleared during the process terminal transition, before the
  exited thread slot can be reused; later process reclaim clears it only when
  it still names that process.
- A process-owned address space is not destroyed until its main thread has been
  reaped and its user stack has been released.
- Stack, ELF-frame, and page-table destruction does not run under scheduler or
  local-IRQ exclusion.
- State, current identity, generation, and ready-queue membership remain owned
  by the scheduler transition layer.

## Consequences

The scheduler has one generation-bearing identity and one lifecycle model.
Context switching no longer needs a process reference, and process wait policy
is layered over generic thread exit/join/reap instead of introducing another
scheduler operation.

The current one-thread-per-process model uses one fixed reverse-index entry per
thread slot. Multiple user threads would require extending that process-owned
membership index; shared address-space thread groups, cancellation, and SMP
synchronization remain future work.

Kernel stacks remain preallocated, so moving ownership between a free slot and
an occupied thread does not add runtime allocation. User-stack and address-space
construction still occurs in process thread context and remains fallible before
publication.

## Alternatives considered

- Keep `Task` as an internal scheduler entity and `Thread` as an external
  wrapper: rejected because two identities obscure direct lookup and lifecycle
  ownership without providing another scheduling abstraction.
- Store `ProcessId` in user threads: rejected because context handoff needs only
  an address-space activation ID; process policy belongs to the process table.
- Keep user stacks in `Process`: rejected because stack lifetime follows the
  executing and exited thread through generic join/reap.
- Add process-specific scheduler join/wait operations: rejected because exact
  generic thread waits already provide the required blocking lifecycle.
- Add new QEMU scenarios for the structural refactor: rejected for this issue;
  pure table invariants are unit-tested and existing contracts exercise the
  observable paths.

## Validation

- Unit-test bounded slot allocation, direct lookup, stale generations, free-slot
  rejection, release/reuse, capacity exhaustion, and wait-sequence survival.
- Run the existing AArch64 `kernel-contract`, `userspace-contract`,
  `shell-contract`, and `user-fault` suites without adding markers or cases.
- Run structural searches for removed `Task` types, scheduler `ProcessId`
  coupling, process-owned stacks, and the legacy `task_call` name.
- Run the canonical host/build and merge gates.

## Partial supersession

- [ADR-0014](ADR-0014-bounded-kernel-thread-lifecycle.md): slot identity and
  stack/context ownership now use the unified `Thread`/`ThreadSlot` model.
- [ADR-0017](ADR-0017-process-table-and-user-fault-policy.md): the main user
  thread is joinable, owns its user stack, and uses generic join/reap; the
  process retains address-space and status ownership.
- [ADR-0022](ADR-0022-fork-exec-waitpid-echo.md): `fork`, `execve`, and `waitpid`
  now transfer or reap user-stack ownership through the main thread.
- [ADR-0031](ADR-0031-centralized-scheduler-state-transitions.md): transition
  terminology and direct slot identity now refer only to threads.
- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md):
  diagnostic `WaitKind` is removed, and active wait metadata belongs to the
  occupied thread while its sequence persists in the slot.

## Related decisions

- [ADR-0027](ADR-0027-typed-active-context-and-syscall-boundary.md)
- [ADR-0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md)
- [ADR-0030](ADR-0030-nested-preemption-control-and-deferred-rescheduling.md)
- [ADR-0031](ADR-0031-centralized-scheduler-state-transitions.md)
- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
