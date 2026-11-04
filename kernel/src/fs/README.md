# Virtual filesystem

The mount-aware VFS separates pathname, descriptor, and process policy from
filesystem backends. The production root is readonly initramfs-backed ramfs;
optional FAT32 backends mount QEMU partitions according to boot `/fstab`, with
`/mnt/disk` as the absent-file fallback. A separate writable/generated test
backend is enabled only for controlled tests.

| Module | Responsibility |
| --- | --- |
| `backend` | Stable nodes, metadata, owned requests, and backend completion contract |
| `vfs` | Boot-built namespace, pinned locations, mount traversal, and resolver state |
| `fd` | Bounded descriptor table, shared open descriptions, and logical I/O leases |
| `io` | Unified descriptor targets and reusable path-independent endpoint I/O |
| `call` | Process-owned syscall restart state, bounded usercopy, and exact waits |
| `ramfs`, `initramfs` | Readonly archive adapter and deterministic boot indexing |
| `fat` | Leased FAT32 operations, stable nodes and sector adaptation |
| `mounts` | First-disk FAT32 configuration lookup and boot mount policy |
| `testfs` (test-only) | Independent writable/generated instances and pending-I/O fixture |

## Namespace and paths

The first ordinary initialization thread builds and publishes the namespace
before spawning EL0. Early boot indexes initramfs but performs no disk I/O.
One global kernel-controlled namespace attaches filesystem instances at existing
directories. Mounts and referenced nodes remain stable; dynamic unmount is not
implemented. Invalid or conflicting registration fails without replacing an
existing mount.
The boot namespace permits at most 16 additional mounts beyond its root.
Mountpoint crossing and conflict checks use filesystem-instance/node identity,
so FAT case and short-name aliases cannot bypass a mounted filesystem.

Current QEMU storage uses the first discovered disk and its first primary FAT32
partition to read `/fstab`; this is not a hardware boot-medium identification
mechanism. Only a missing file selects `/mnt/disk`; a present
file controls all automatic mounts, including an empty table. Parsing and mount
application happen before namespace publication. Entries name partitions with
`/dev/diskNpM` boot selectors, support readonly policy and require existing
mountpoint directories. See [storage](../../../docs/storage.md) for the format.

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

`Location` pins the mount and node that an operation targets. Call
`location.poll(&mut request, wake)`; it forwards the instance-local node to
`Backend::poll` separately. `Request` owns only the operation, staging and
continuation, never a second target identity. Retain the same location across
`Pending` resumes. For lookup/create that location is the parent directory;
the returned child node becomes a new location after completion.

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

Each process retains a fixed 32-entry FD table. Entries refer either to VFS open
descriptions or to I/O objects without a VFS location. A VFS description owns
access mode, backend open state, and file/directory position; an I/O object
encapsulates endpoint state and read/write completion. The initial process gets
UART stdin/stdout/stderr at 0, 1, and 2, but these slots are ordinary replaceable
entries. Open and dup choose the lowest free slot, including 0 after close.

`dup` shares the source reference. `dup2` validates both numbers and atomically
replaces the destination, returning the displaced reference for unlocked
destruction. Valid self-duplication succeeds without changing ownership;
invalid input leaves the destination intact. Kernel logs use the kernel
console independently of any process's stdout.

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
insufficient. File read/write accept the requested byte count, with no fixed
usercopy cap; unrepresentable counts fail with `EINVAL`, failed buffer growth
with `ENOMEM`. Generic request/staging work avoids ephemeral small-I/O allocation:
private I/O state is prepared during open and generated output uses bounded storage.
The resident FAT library is an explicit exception for small cache/run metadata
allocations in IRQ-enabled task context; it also retains volume-sized caches.
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
Non-filesystem endpoints use the same exact-wait/restart lifecycle. UART input
registers its empty-ring wait before returning Pending; user buffers are
validated before consuming received bytes. Endpoint dispatch is based on the
FD entry, never on whether its number happens to be 0, 1, or 2.
UART reads return at most the natural 256-byte RX capacity. Writes use retained
staging and enqueue at most 256 bytes per IRQ-masked TX section, returning a
confirmed prefix only on queue pressure. Large writes do not become silently
truncated by a new fixed syscall byte limit.

Finite allocation, submission, and destruction may use an IRQ-enabled task
window while protecting the borrowed live exception frame from preemption.
That exclusion ends before a blocking handoff. Future drivers must complete
through the pending-operation contract rather than block under a spinlock or
retain an `ActiveContext`.

## Syscalls and verification

`read` and `write` dispatch through the resolved FD entry; filesystem operations
continue through VFS. `SYS_DUP=21` and `SYS_DUP2=22` provide descriptor
duplication. `SYS_FSYNC=20` adds explicit durability; withdrawn number
19 stays unknown. Writable open flags are supported
where the backend supports them; ramfs remains readonly. File transfers use
the full caller-specified count through retained staging; a backend can still
return partial progress or EOF. Directory calls emit one naturally bounded
record using the existing Linux-like layout and entry-cookie semantics.

FAT retains one resident filesystem and one canonical library file per stable
node under an unlocked logical volume lease. Independent OFDs share current
file metadata, not positions; append selects EOF in that same lease. Payload
I/O uses the retained handle instead of reopening by path. Explicit fsync
publishes FAT/FSInfo then flushes the block device; it does not unmount. No disk
operation occurs on final OFD close. Partitions sharing a physical disk acquire
its exclusive owner before library work; contention returns `Pending` with an
exact token, never a nested blocking wait or a held disk spinlock. Readonly
mounts reject mutations and do not publish FAT metadata on sync. Timeouts belong
to the block driver; FAT has no operation-wide call/time budget or hard latency
guarantee. See
[storage](../../../docs/storage.md) and
[ADR-0067](../../../memory/decisions/ADR-0067-resident-fat-and-canonical-file-handles.md).

Host tests exercise namespace, operation, offset, error, and lifetime contracts.
`vfs-contract` adds controlled mounts, generated and writable files, cross-CPU
shared descriptions, process inheritance, cleanup, and pending completion.
Deferred mutation fixtures retain completed replies across real syscall restart
and verify exact create/open/truncate/write counts and peer progress.
Pending-read fixtures use an explicit controller release rather than a timed
completion; abandoned requests detach their exact wait registration.
Its backend, fixtures, and protocol markers are test-only. Ordinary userspace,
shell, SMP, and release checks continue to exercise the production ramfs root.

Procfs, Pi SDHOST, dynamic unmount, links, permissions, metadata
syscalls, terminal integration, and a page cache remain separate work.

See [ADR-0062](../../../memory/decisions/ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md)
for ownership and supersession of the earlier ramfs-only descriptor/cwd model.
See [ADR-0071](../../../memory/decisions/ADR-0071-unified-descriptors-and-shell-redirection.md)
for unified endpoint references and ordinary stdio slots.
