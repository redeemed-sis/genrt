# ADR-0073: Best-effort init mounts

## Status

Accepted

Partially supersedes [ADR-0072](ADR-0072-runtime-mounts-and-userspace-init.md)
for fatal mount failures and the comments-only default fstab.

## Context

Optional data storage must not prevent access to the initramfs shell when a
disk, partition or mountpoint is unavailable.

## Decision

- Configure `/dev/disk0p1` at `/mnt/disk` and `/dev/disk0p2` at
  `/mnt/disk/DATA` as read-write vfat in the production initramfs fstab.
- Report each failed mount to stderr with its configuration line, source,
  target and errno. Attempt later automatic entries and exec `/bin/shell`
  afterward, including when every mount fails. Do not select fallback sources.
- Preserve whole-file validation, noauto, entry order and bounds. Malformed
  or unreadable fstab remains an initialization error; missing/empty fstab
  requests no mounts. Earlier successful mounts remain installed.

## Invariants

Mount policy stays in userspace. Kernel mount ABI, partition claims, atomic
publication, readonly enforcement and explicit fsync durability are unchanged.

## Consequences

Diskless systems start the shell with mount warnings. Empty/comments-only
configuration suppresses the attempts. Applications must handle unavailable
data paths; a warning is not evidence of successful mounting.

## Alternatives considered

Abort on the first mount error: prevents shell access for optional-storage
failures. Silently ignore errors or skip later entries: hides diagnostics or
prevents independent mounts. Automatic fallback changes explicit source policy.

## Validation

Host tests check first, second and all mount failures, warning contents/stderr,
continued attempts and shell exec. A diskless QEMU contract executes the exact
initializer with two failing entries; the storage contract preserves successful
nested mounts and persistence checks. Report executed gates separately.

## Related decisions

- [Storage guide](../../docs/storage.md)
