# ADR-0043: Process affinity query ABI

## Status

Accepted

## Context

ADR-0042 gave each userspace process one immutable logical CPU owner and added
one-shot child placement. Userspace could request the owner of a future child,
but it could not observe the resulting process, main-thread, and address-space
ownership through a production ABI. Tests therefore exercised remote lifecycle
without directly verifying the selected affinity from EL0.

The kernel CPU capacity is an implementation limit and must not become part of
the userspace ABI. General mutable affinity would also require process
migration and a userspace TLB-shootdown protocol that do not exist.

## Decision

- Add syscall number 12 for
  `sched_getaffinity(pid_t pid, size_t cpusetsize, cpu_set_t *mask)`.
- Define a fixed 1024-bit userspace `cpu_set_t`, independently of
  `KERNEL_CPU_CAPACITY`, with `CPU_ZERO`, `CPU_SET`, `CPU_CLR`, and `CPU_ISSET`.
- PID `0` selects the current process. A positive generation-bearing PID may
  select another live or zombie process until that process is reaped.
- Require `cpusetsize >= sizeof(cpu_set_t)`. On success, clear and write exactly
  one `cpu_set_t`, set only the immutable owner bit, and return zero. A larger
  caller buffer is accepted but bytes beyond `cpu_set_t` are unchanged.
- Return `ESRCH` for a negative, malformed, stale, absent, or reaped PID,
  `EINVAL` for a short CPU set, and `EFAULT` for an invalid writable range.
- Treat a live process without CPU ownership, or an owner beyond the userspace
  CPU-set ABI, as a kernel invariant violation rather than inventing a mask.
- Copy the owner while holding the process table's IRQ-safe lock, release the
  lock, construct the fixed mask on the kernel stack, and use the existing
  user-copy layer for the result.
- Do not add `sched_setaffinity`. Process placement remains immutable after
  publication; only the existing next-fork policy can select another CPU.

## Invariants

- The single returned bit equals
  `process.owner_cpu == main_thread.home_cpu == address_space.owner_cpu`.
- The query never derives affinity from the executing CPU, pending fork policy,
  scheduler queue location, or kernel capacity.
- Process-table ownership is not held during userspace validation or copying.
- The syscall performs no allocation or blocking and does not mutate process,
  scheduler, address-space, or affinity state.

## Consequences

Userspace and QEMU contracts can verify fork placement and exec preservation
through production ABI alone. The fixed 128-byte mask is larger than the
current topology needs, but it is stable if kernel CPU capacity changes.

The interface deliberately provides only process-level, single-owner affinity.
Supporting migration, multiple user threads with different homes, or address
spaces active on several CPUs requires a later decision and a different memory
coherence protocol.

## Alternatives considered

- Return a raw logical CPU index: rejected because it does not match the
  requested Linux-shaped API and cannot grow to multi-CPU affinity.
- Size `cpu_set_t` from `KERNEL_CPU_CAPACITY`: rejected because it exposes a
  kernel build-time limit through userspace ABI.
- Infer affinity from the CPU executing the syscall: rejected because queries
  for another process and future migration semantics require owned metadata.
- Add `sched_setaffinity`: rejected because immutable placement is the premise
  that keeps TTBR0 invalidation owner-local without general shootdown.

## Validation

- The single-CPU userspace contract checks `/init` affinity, the CPU-set macros,
  current and child queries, post-reap rejection, short buffers, invalid
  pointers, invalid PIDs, and noncanonical raw PID aliases.
- The four-CPU userspace contract checks child self-query, parent query by PID,
  one-shot/default/replaced/reset placement, secondary CPU ownership, exec
  preservation, and remote fault/reap behavior.
- The canonical `cargo xtask ci` gate retains all existing scheduler, fault,
  shell, userspace, and release-composition coverage.

## Related decisions

- [ADR-0027](ADR-0027-typed-active-context-and-syscall-boundary.md)
- [ADR-0033](ADR-0033-unified-thread-model-and-scheduler-process-separation.md)
- [ADR-0042](ADR-0042-smp-userspace-memory-and-fork-affinity.md)
