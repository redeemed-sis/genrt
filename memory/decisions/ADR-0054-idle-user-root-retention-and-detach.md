# ADR-0054: Idle user-root retention and terminal detach

## Status

Accepted

Supersedes [ADR-0042](ADR-0042-smp-userspace-memory-and-fork-affinity.md) only
for unconditional TTBR0 activation/clear invalidation and idle-root handoff.
Immutable process/thread/address-space CPU ownership remains unchanged.

## Context

A periodic user process commonly blocks into permanent idle, then resumes the
same immutable address space. Clearing and reinstalling its root discards local
translations despite there being no new userspace mapping or owner. Simply
skipping clear would be unsafe if destruction still assumed a kernel current
thread meant that no user root remained installed.

## Decision

- Track the installed physical user root per CPU in the generic VM owner.
  Publish changes only after architecture activation or detach completes.
- Repeated activation of the same installed immutable root is a no-op. The
  AArch64 adapter also checks actual TTBR0 before writing it. Changing roots
  or detaching a nonzero root still performs the existing local invalidation
  and barriers; clearing an already empty root is a no-op.
- Permanent idle may retain TTBR0. Its trusted high-half code must not read or
  write user pointers. Kernel interrupt handlers do not acquire userspace
  execution rights merely because a root remains installed.
- Preserve the active-root record during idle. Address-space destruction
  rejects any root still recorded active; retention is not ownership release.
- Other kernel threads clear the user root. A different user thread activates
  its own owner-local root normally. There are no ASIDs, migration, shared
  userspace roots across CPUs, or reduced isolation checks.
- Terminal user exit to idle explicitly detaches before exception return can
  deliver the retired-thread notification and publish join/reap. Exit to a
  different user root or ordinary kernel thread detaches through normal
  activation. Resource cleanup therefore never frees the installed root.

## Invariants

- Root identity, immutable owner CPU, and hardware installation agree.
- Retained idle roots remain live until detach; frame reuse cannot precede it.
- Idle has no user-pointer contract or process operation capability.
- Exec replacement, A-to-B handoff, and user exit preserve local invalidation
  before old address-space destruction.
- Same-root detection and handoff remain bounded and allocation-free.

## Consequences

Repeated user/idle sleep cycles keep translations without adding an ASID or
shootdown policy. Ordinary kernel-thread work still pays detach/activation.
This optimizes only actual same-root reuse, not all context switches. A future
idle routine using user memory would violate this contract and require review.

## Alternatives considered

- Always detach is simple but repeats invalidation for unchanged mappings.
- ASIDs would address a wider multi-process performance problem and require
  allocation, reuse, and invalidation lifetime rules outside this milestone.
- Retaining roots through every kernel thread would broaden user-pointer and
  resource-lifetime obligations unnecessarily.

## Validation

The kernel contract joins a sole EL0 probe through 32 sleep/idle cycles per
process and requires exactly two actual root writes: initial activation and
terminal detach. Three complete lifetimes return all frames after reap.
Production-kernel contracts exercise concurrent process data isolation, fork,
exec replacement, secondary-CPU sleeps, remote fault, and exit/reap. Test root
instrumentation is absent from production builds.

## Related decisions

- [ADR-0042](ADR-0042-smp-userspace-memory-and-fork-affinity.md)
- [ADR-0040](ADR-0040-per-cpu-scheduler-activation.md)
- [ADR-0055](ADR-0055-batched-timer-wakeup-and-idle-selection.md)
