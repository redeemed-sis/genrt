# ADR-0071: Unified descriptors and shell output redirection

## Status

Accepted

Supersedes the reserved stdio slots in
[ADR-0019](ADR-0019-readonly-ramfs-and-fd-table.md) and the stdin-specific
syscall restart/registration path in
[ADR-0020](ADR-0020-uart-stdin-and-shell.md). Extends the description ownership
and exact-wait model in
[ADR-0062](ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md).
Replaces the shell-redirection non-goal in
[ADR-0022](ADR-0022-fork-exec-waitpid-echo.md).

## Context

Filesystem descriptors already share positions across fork and survive exec,
but read/write on descriptor numbers 0, 1, and 2 bypass the FD table. Reserved
console numbers prevent ordinary close, duplication, and shell redirection.
Future terminals, sockets, and pipes need endpoints without filesystem paths;
inventing a VFS location for each endpoint would mix namespace and I/O policy.

## Decision

- One bounded FD table owns references to either VFS open descriptions or
  reference-counted I/O objects. VFS locations, backend requests, and shared
  offset transactions retain their existing responsibilities. I/O objects
  expose read/write completion without requiring a mount, node, or path.
- The first process receives UART input/output objects at 0, 1, and 2.
  Thereafter these are ordinary slots: close can release them, open and dup
  choose the lowest available slot including 0, and dup2 can replace them.
  Kernel diagnostics continue to use the kernel console, not process stdout.
- Read/write dispatch resolves the descriptor entry, not its numeric value.
  Pending endpoints use the same prepared exact token and process-owned
  restart state as filesystem calls. Endpoint code publishes/rechecks its
  condition without retaining a borrowed exception frame or blocking under
  a task window. UART receive remains bounded and IRQ-driven.
  UART writes retain the requested count and enqueue in chunks of at most
  256 bytes, preserving short IRQ-masked sections and confirmed partial progress
  on queue pressure rather than imposing a new 256-byte write limit.
- `SYS_DUP=21` and `SYS_DUP2=22` return the new descriptor or negative errno.
  Number 19 remains withdrawn and `SYS_FSYNC=20` is unchanged. Dup shares the
  same description/endpoint; VFS offsets are not copied. Dup2 validates the
  source and destination before atomically replacing the destination, returns
  it unchanged for a valid self-duplication, and never closes the destination
  when validation fails. A full table makes dup fail with `EMFILE`; an invalid
  descriptor makes either call fail with `EBADF`.
- Fork clones references; exec preserves them. Table ownership covers only
  bounded clone/move/replace operations. Replaced resources are returned to
  task context for final release after the process lock is gone.
- The shell accepts external `command > file` and `command >> file`.
  Only the child opens with create plus truncate/append, duplicates onto
  stdout, closes a distinct temporary descriptor, and execs. The parent keeps
  its original descriptors. Syntax errors, unsupported builtin redirection,
  open failure, and duplication failure are explicit errors. Parsing remains
  bounded; quoting, pipelines, input/stderr redirection, and job control are
  not added.

## Invariants

- No endpoint callback, usercopy, allocation, or final resource destruction
  runs under IRQ-disabled process/scheduler ownership.
- Ordinary small read/write calls reuse provisioned storage. Pending state
  owns its input and wait identity, never a syscall-stack buffer.
- Duplication never allocates a second filesystem position or endpoint state.
- Replacing stdout in a child cannot modify the parent's FD table.
- UART IRQ delivery and scheduler handoff remain allocation-free.

## Consequences

The same utilities can write to UART or FAT32 without changing their write or
printf calls. Descriptor numbers no longer imply resource type or access mode.
An open after closing stdin can return 0; callers must not assume opens always
return at least 3. This adds no devfs, terminal discipline, socket, or pipe.
Shell redirection is not an implicit durability operation: applications needing
durability must call fsync, and tests explicitly sync before poweroff.

## Alternatives considered

- Keep special stdio numbers and add a separate redirect flag: rejected because
  ordinary descriptor replacement and inheritance would still be inconsistent.
- Represent every endpoint as a VFS backend: rejected because I/O ownership
  must not require a namespace location.
- Configure the parent's stdout before fork: rejected because failure paths
  could leave the interactive shell redirected.

## Validation

Focused host tests cover descriptor ownership, slot reuse, shared positions,
duplication/replacement, pending endpoint I/O, and bounded shell parsing.
Userspace QEMU contracts exercise the ABI and fork/exec inheritance; the UART
shell contract covers blocking stdin and editing. The two-boot FAT32 storage
contract runs the production shell, verifies redirected write/printf output,
and checks exact file contents again after restarting the same disk image.
These functional checks do not establish hardware latency or power-loss safety.

## Related decisions

- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [ADR-0063](ADR-0063-reusable-open-description-io-storage.md)
- [Filesystem ownership](../../kernel/src/fs/README.md)
- [Userspace](../../user/README.md)
