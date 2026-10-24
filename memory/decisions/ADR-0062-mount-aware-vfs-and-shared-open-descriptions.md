# ADR-0062: Mount-aware VFS and shared open file descriptions

## Status

Accepted

Supersedes the ramfs-specific descriptor representation in
[ADR-0019](ADR-0019-readonly-ramfs-and-fd-table.md), independent fork offsets in
[ADR-0022](ADR-0022-fork-exec-waitpid-echo.md), directory-description ownership in
[ADR-0023](ADR-0023-directory-fds-and-getdents64.md), and ramfs-only cwd/path
identities in [ADR-0024](ADR-0024-process-cwd-and-path-resolution.md).
The existing syscall numbers, stdio policy, archive backing, and directory
record ABI remain unchanged.

## Context

Filesystem syscalls previously traversed a global immutable ramfs index.
Descriptors and cwd retained backend-specific indexes, and fork copied offsets.
Future generated files and persistent storage need a common namespace and
backend contract without adding filesystem-specific branches to syscall code.

An EL0 syscall owns a live exception frame. A blocking backend cannot simply
sleep beneath a spinlock or an IRQ-enabled preemption guard while that frame
is borrowed. Shared descriptions also make separate offset observation and
advancement racy across independently placed parent and child processes.

## Decision

- Introduce one kernel-controlled mount namespace. The initramfs-backed ramfs
  is its readonly root. Additional instances attach at existing directories;
  mounts and node identities remain stable, and dynamic unmount is deferred.
- Resolve every pathname component through the backend boundary, preserving
  missing-component and non-directory errors before applying later `..`.
  Traversal follows mounts and crosses back to the namespace parent from a
  mounted root. Cwd uses a mount-aware directory reference. Components have a
  common 255-byte limit, checked before lookup or creation and during archive
  indexing, so accepted names remain enumerable within bounded usercopy.
- Define backend operations for lookup, open/release, read/write, directory
  iteration, creation/truncation, seeking, synchronization, and metadata.
  Unsupported operations return errors. Generated reads need not retain
  permanent file-data slices. Ramfs keeps its archive-backed zero-copy data
  and deterministic directory ordering.
- Keep the bounded per-process descriptor table. Its entries own references
  to open file descriptions; access mode, backend open state, and file or
  directory position belong to the shared description. Fork clones references,
  so offsets are shared. Independent opens create independent descriptions.
  Exec preserves descriptors and cwd.
- Process critical sections only clone, install, detach, or move references.
  Backend calls, allocation, userspace copies, and last-reference destruction
  occur outside process/scheduler locks in task context. Exit, fault, and failed
  process staging use the same detach-before-destroy ownership rule.
- Serialize one description's in-flight I/O without holding a spinlock over
  backend work or userspace copy. Append chooses EOF and writes atomically in
  the backend, including writes through different descriptions of one node.
- Blocking-capable I/O uses a restartable pending-operation contract and exact
  scheduler waits. Pending state owns its buffers and progress rather than
  borrowing the syscall stack. Completion is rechecked during registration;
  the syscall restarts after wakeup without repeating committed side effects.
  Preemption exclusion protects only finite submission/cleanup work and ends
  before a blocking handoff.
- Keep existing filesystem syscall numbers and bounded userspace-copy sizes.
  Large transfers proceed through partial calls. Writable flags are interpreted
  by VFS/backend policy; the production root remains readonly. Executing ELF
  from backends other than ramfs is not required by this decision.

## Invariants

- Mount, node, description, request, and process lifetimes have explicit owners.
- No allocation, backend callback, usercopy, or final destruction occurs under
  IRQ-disabled process or scheduler ownership.
- Active scheduler/wakeup paths remain bounded and allocation-free. An I/O wait
  cannot block under a preemption guard or spin by polling for another owner.
- Shared offsets advance only for committed successful transfers; failed
  userspace copies do not consume file bytes or directory entries.
- Pending retries retain exact operation identity and do not duplicate writes,
  creation, truncation, or other completed effects.
- Test backends, mount fixtures, and protocol markers never enter production
  kernel or initramfs artifacts.

## Consequences

Existing shell and ramfs ELF execution use the VFS namespace. Fork offset
sharing is an intentional compatibility change toward POSIX semantics.
Production boot still installs no writable or persistent filesystem. Procfs,
FAT32, storage drivers, permissions, links, and userspace mount/unmount are
separate work; none is needed to exercise the common backend interface.

## Alternatives considered

- Extend ramfs indexes with writable and generated-file special cases: rejected
  because it preserves syscall/backend coupling.
- Copy open descriptions on fork: rejected because shared file positions are
  an explicit requirement.
- Hold a spinlock across synchronous backend callbacks: rejected because it
  prevents blocking and extends scheduler/preemption exclusion across I/O.
- Enable IRQs and block beneath a borrowed EL0 frame: rejected without a proven
  continuation ownership model; existing exact-wait syscall restart is used.

## Validation

Host filesystem tests exercise mount validation, component traversal, backend
operations, shared positions, errors, and lifetime behavior. The four-CPU
`vfs-contract` uses only feature-gated backends and controlled fixtures to
exercise userspace traversal, generated/writable I/O, process inheritance,
concurrent access, and pending completion. Existing userspace, shell, fault,
SMP, and release gates remain regression checks. QEMU checks are functional,
not physical storage or real-time latency evidence.

## Related decisions

- [ADR-0021](ADR-0021-initramfs-cpio-root.md)
- [ADR-0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md)
- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [Filesystem guide](../../kernel/src/fs/README.md)
