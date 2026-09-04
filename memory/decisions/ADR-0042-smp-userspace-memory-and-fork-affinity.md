# ADR-0042: SMP userspace memory and fork affinity

## Status

Accepted

## Context

ADR-0040 activated a scheduler context on every registered CPU, and ADR-0041
made remote runnable publication immediate through targeted scheduler SGIs.
Userspace still relied on an implicit CPU0 boundary: an address-space identity
did not record where it could be activated, process publication could expose a
remote child before the process reverse index was bound, and no userspace API
could select a future child's CPU.

General migration or one address space executing on several CPUs would require
ASIDs, shootdown commands, and broader lifetime rules. The current process
model has one immutable-affinity userspace thread, so independent processes can
use secondary CPUs with a smaller ownership contract.

## Decision

- Give every live process one immutable logical `CpuId`. Its main thread's
  `home_cpu` and its address-space owner must be identical.
- Split TTBR0 lifecycle types. `StagedUserAddressSpace` is unpublished and
  mutable; ELF, stack, fork-copy, and exec preparation map only through it.
  Assigning a future CPU consumes the stage and yields an immutable
  `OwnedUserAddressSpace` plus a copyable owner-bearing `AddressSpaceId`.
- Build a forked address space on the calling CPU, then assign the selected
  future owner before scheduler publication. VM staging does not depend on the
  default placement algorithm.
- Hold process-table ownership while scheduler publication allocates the exact
  generation-bearing `ThreadId`. A bounded infallible pre-ready hook validates
  the owner triple and binds the reverse index before the scheduler appends the
  thread to a local ready queue or remote ingress. The scheduler stores no
  `ProcessId` and the hook cannot reenter scheduler APIs.
- Add the genrt syscall `sched_setforkaffinity(int cpu)`. A nonnegative value
  replaces the calling process's pending next-child CPU after validating that
  it is registered and scheduler-online. `-1` resets to default placement;
  values below `-1` fail with `EINVAL`.
- Default fork placement is the parent's immutable owner. Explicit and default
  placement use the same staging, owner assignment, and publication path.
- Consume pending placement only in the successful pre-ready publication hook.
  Failure before publication preserves it. Every new child starts with no
  pending setting, and changing it never changes the parent or an existing
  child.
- Preserve process ownership across `execve`; the replacement stage receives
  the existing owner before commit.
- Permit TTBR0 activation only when the executing CPU equals the immutable
  address-space owner. Track the active root for each logical CPU and reject
  destruction while any active record names it.
- Keep published userspace mappings immutable. Staged mapping needs no TLB
  invalidation because the root has never been active. AArch64 TTBR0 activation
  and clear use local `VMALLE1` because a root has one CPU owner and there are
  no ASIDs. Shared runtime TTBR1 updates retain inner-shareable broadcast TLBI.
- Preserve lifecycle ordering: thread exit prevents future resume, generic
  join/reap releases the user stack and saved context, then process cleanup
  frees ELF frames and destroys the inactive root outside process and scheduler
  locks.

## Invariants

- `process.owner_cpu == main_thread.home_cpu == address_space.owner_cpu` for
  every published userspace process.
- Owner assignment and process reverse-index binding complete before runnable
  visibility. A remote scheduler IPI cannot race ahead of process ownership.
- One published address space is immutable, active on at most its owner CPU,
  and never activated or destroyed by a foreign CPU.
- Pending fork affinity is process creation policy. Successful publication is
  its only consume point; failed pre-publication work cannot clear it.
- Scheduler and architecture layers never choose default process placement.
  Scheduler validates thread/address-space agreement and AArch64 implements
  local versus broadcast invalidation.
- Process-to-scheduler publication is bounded and allocation-free while both
  owners are held. Destructive VM and frame cleanup happens after those guards
  are released.

## Consequences

Independent processes can run on different CPUs and use existing remote-ready
IPI delivery without a general userspace shootdown protocol. Fork may incur the
same eager-copy allocation and copy cost on the parent CPU regardless of child
placement. Switching processes without ASIDs invalidates local stage-1
translations, including translations not belonging to TTBR0, which is correct
but deliberately coarse.

The design does not support migration, shared address spaces across CPUs, or
multiple user threads with different affinity. Any such feature must replace
the immutable-owner premise and add an explicit shootdown/lifetime protocol.

This decision supersedes only ADR-0040's CPU0-only userspace boundary. Its
per-CPU scheduler ownership and the targeted IPI protocol from ADR-0041 remain
unchanged.

## Alternatives considered

- Add runtime `sched_setaffinity`: rejected because moving a published thread
  and active root requires migration and shootdown ownership outside this
  milestone.
- Construct remote child mappings on the target CPU: rejected because staged
  page tables are inactive data and need no target-CPU execution.
- Add general userspace TLB shootdown now: rejected because immutable
  single-CPU ownership makes it unnecessary and keeps invalidation bounded.
- Publish the scheduler thread, then bind the process table: rejected because a
  targeted SGI could execute the child before current-process lookup exists.
- Store `ProcessId` in scheduler state: rejected because it reverses the
  scheduler/process boundary established by ADR-0033.

## Validation

- Host process tests cover default selection, replacement, reset, successful
  consumption, failure preservation, child non-inheritance, and owner mismatch
  rejection.
- The single-CPU `userspace-contract` rejects unavailable and invalid CPU
  values while retaining the production syscall path.
- The four-CPU `smp-userspace-contract` uses the production kernel and checks
  explicit, one-shot, replacement, reset, bounded-exhaustion retry, remote
  exec, remote fault, and cross-CPU wait/reap behavior.
- Existing `smp-boot`, user-fault, shell, product, and release contracts remain
  part of the canonical `cargo xtask ci` gate.

## Related decisions

- [ADR-0022](ADR-0022-fork-exec-waitpid-echo.md)
- [ADR-0033](ADR-0033-unified-thread-model-and-scheduler-process-separation.md)
- [ADR-0036](ADR-0036-smp-safe-runtime-synchronization.md)
- [ADR-0040](ADR-0040-per-cpu-scheduler-activation.md)
- [ADR-0041](ADR-0041-targeted-scheduler-ipi-and-remote-wakeup.md)
