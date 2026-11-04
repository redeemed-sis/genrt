# ADR-0065: Explicit fsync syscall

## Status

Accepted

Extended by [ADR-0071](ADR-0071-unified-descriptors-and-shell-redirection.md):
descriptor numbers 0 through 2 no longer imply console or fsync policy. UART
endpoints return `EINVAL`; VFS descriptions support sync regardless of slot.

## Context

The VFS already exposes backend `Operation::Sync`, but userspace cannot request
a durable storage boundary. Closing a shared description must not implicitly
perform filesystem I/O, and neither process exit nor a power command replaces
an explicit successful synchronization.

## Decision

Add `SYS_FSYNC = 20`, `x0 = fd`, result zero or negative errno. Number 19 remains
withdrawn/unknown. The C `fsync(int fd)` wrapper uses the same ABI as ordinary
descriptor operations. Std streams and absent descriptors return `EBADF`.

Dispatch through the existing restartable VFS call and OFD transaction, without
user pointers or new staging. Backends may pend through exact waits. Ramfs sync
is a successful no-op. FAT sync serializes metadata publication and performs
the underlying block-device flush after consuming the temporary filesystem.
Failure propagates; unsupported durability returns `ENOTSUP`, never success.

## Invariants

No sync I/O or destruction under process, scheduler, OFD or volume spinlocks.
Fork-shared descriptions serialize sync with their I/O. Backend close remains
non-I/O cleanup. A successful prefix of a failed write is not replayed by sync.

## Consequences

This is a genrt ABI number, not Linux's syscall-number assignment. No global
sync syscall, crash-consistency guarantee, permissions model or implicit
poweroff flushing is added.

## Alternatives considered

Sync-on-close makes destructor context perform potentially failing I/O and
cannot report a shared-description durability boundary reliably. A FAT-specific
syscall would leak backend policy into the generic ABI.

## Validation

The production-kernel userspace contract checks valid ramfs fsync and invalid
descriptors. The storage contract writes and fsyncs FAT, restarts QEMU on the
same image and independently verifies the payload through mtools. Host tests
inject device-sync and earlier metadata failures.

## Related decisions

- [ADR-0062](ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md)
- [ADR-0064](ADR-0064-qemu-block-storage-and-fat32.md)
