# Virtual filesystem

The mount-aware VFS separates pathname, descriptor, and process policy from
filesystem backends. The production root is the readonly initramfs-backed
ramfs; a writable/generated test backend is enabled only for controlled tests.

| Module | Responsibility |
| --- | --- |
| `backend` | Stable nodes, metadata, owned requests, and backend completion contract |
| `vfs` | Boot-built namespace, pinned locations, mount traversal, and resolver state |
| `fd` | Bounded descriptor table, shared open descriptions, and logical I/O leases |
| `call` | Process-owned syscall restart state, bounded usercopy, and exact waits |
| `ramfs`, `initramfs` | Readonly archive adapter and deterministic boot indexing |
| `testfs` (test-only) | Independent writable/generated instances and pending-I/O fixture |

## Namespace and paths

One global kernel-controlled namespace attaches filesystem instances at existing
directories. Mounts and referenced nodes remain stable; dynamic unmount is not
implemented. Invalid or conflicting registration fails without replacing an
existing mount.
The boot namespace permits at most 16 additional mounts beyond its root.

Path traversal is component-based, absolute or relative to mount-aware cwd.
`.` remains in the current directory. At a mounted root, `..` crosses to the
mountpoint's namespace parent; at namespace root it stays at `/`. A missing
component or a regular file used as a directory fails before any subsequent
`..` can erase the error. Trailing directory syntax and `GENRT_PATH_MAX` remain
enforced. Components are limited to 255 bytes before lookup or creation so every
accepted name fits a bounded directory record. Backends and archive indexing
enforce the same limit. Cwd reports a namespace path, not a backend-local path.

Mount and node identity is filesystem-independent outside backend adapters.
Executing ELF is still limited to archive-backed ramfs files; other backends
need not expose permanent file-data slices.

## Backend contract

Backends provide lookup, open/release, sequential read/write, directory
enumeration, creation/truncation, seeking, synchronization, and basic metadata.
Unsupported operations return explicit errors. Generated reads fill bounded
caller-owned buffers rather than requiring an allocated whole-file image.
Append selects EOF and writes atomically at backend scope, including concurrent
descriptions referring to the same file.

`initramfs` parses the permanently reserved `newc` archive during boot. Ramfs
retains its immutable indexes, rejects duplicate/conflicting archive paths,
and enumerates immediate children in deterministic sorted order. File bytes
continue to borrow the original archive, without copying them into VFS storage.

## Descriptors and lifetimes

Each process retains a fixed 32-entry FD table. Descriptors 0, 1, and 2 preserve
their console policy; ordinary opens choose the lowest free slot from 3.
Descriptor references are distinct from open file descriptions, which own access
mode, backend open state, and file/directory position.

Fork clones description references, intentionally sharing offsets between
parent and child. Independent opens retain independent positions. Exec preserves
descriptors and cwd. Close detaches one descriptor; final reference release
finalizes backend open state once. Exit, faults, and failed process staging
detach owning file resources before cleanup.

Process/scheduler locks cover only bounded ownership transitions. No backend
callback, allocation, usercopy, or last-reference destruction runs beneath them.
Description serialization likewise does not hold a spinlock over backend I/O
or userspace copies. Failed copies do not consume offsets.

### Reusable I/O storage

Open provisions one reusable backend request with at least 4096 bytes of
staging per OFD. Read/write transactions lease that storage instead of allocating
a fresh vector or boxed call state. There is one staging buffer, not a buffer
in both the transaction and its request. Fork peers share and serialize use of
the same storage; independent opens own separate storage.

Write input is copied only after acquisition, once before submission. Pending
retries retain the request and input. Completion and errors return the storage
without shrinking capacity. A backend reset hook clears per-operation progress
outside owner locks before the request becomes available to another caller.
Growth is allowed only beyond 4096 bytes when the retained capacity is
insufficient; the existing syscall usercopy limit remains
1024 bytes. Small backend reads/writes also avoid ephemeral allocation: private
I/O state is prepared during open and generated output uses bounded storage.
The test backend preallocates mutable backing at creation; growth of file
contents beyond 4096 bytes remains explicit.

See [ADR-0063](../../../memory/decisions/ADR-0063-reusable-open-description-io-storage.md)
for allocation, reuse, and footprint policy.

## Blocking I/O

Backend submission and completion are separate from scheduler handoff. A
pending operation retains owned request state and bounded buffers, not pointers
into a syscall's temporary stack. The condition owner publishes an exact
`WaitToken` and rechecks completion to avoid lost wakeups. The syscall restarts
after notification and consumes the original operation's progress without
duplicating committed side effects.
The one-user-thread-per-process model permits one retained filesystem call per
process; requests are not inherited by fork or carried into a new exec image.

Finite allocation, submission, and destruction may use an IRQ-enabled task
window while protecting the borrowed live exception frame from preemption.
That exclusion ends before a blocking handoff. Future drivers must complete
through the pending-operation contract rather than block under a spinlock or
retain an `ActiveContext`.

## Syscalls and verification

`open`, `read`, `write`, `close`, `getdents64`, `chdir`, and `getcwd` dispatch
through VFS without adding syscall numbers. Writable open flags are supported
where the backend supports them; ramfs remains readonly. File transfers use
at most `MAX_USER_COPY` bytes per syscall, so larger files require repeated
partial calls rather than a whole-file allocation. Directory records retain
the existing Linux-like layout and entry-cookie semantics.

Host tests exercise namespace, operation, offset, error, and lifetime contracts.
`vfs-contract` adds controlled mounts, generated and writable files, cross-CPU
shared descriptions, process inheritance, cleanup, and pending completion.
Deferred mutation fixtures retain completed replies across real syscall restart
and verify exact create/open/truncate/write counts and peer progress.
Its backend, fixtures, and protocol markers are test-only. Ordinary userspace,
shell, SMP, and release checks continue to exercise the production ramfs root.

Procfs, FAT32, storage drivers, dynamic unmount, links, permissions, metadata
syscalls, terminal integration, and a page cache remain separate work.

See [ADR-0062](../../../memory/decisions/ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md)
for ownership and supersession of the earlier ramfs-only descriptor/cwd model.
