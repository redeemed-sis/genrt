# ADR-0072: Runtime mounts and userspace initialization

## Status

Accepted

Partially superseded by [ADR-0073](ADR-0073-best-effort-init-mounts.md) for fatal
mount failures and the comments-only default fstab. Other ownership and ABI
decisions remain unchanged.

Partially supersedes [ADR-0070](ADR-0070-fstab-boot-mounts-and-shared-disks.md)
for disk-root configuration, fallback placement and pre-EL0 mount policy, and
[ADR-0062](ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md) for boot-only
namespace mutation. Shared physical owners, canonical FAT handles, stable node
identity and exact waits remain unchanged.

## Context

Bootstrap previously consumed the disk registry, read fstab from FAT32, mounted
filesystems and froze the namespace. That mixed configuration policy with
mechanism and made unused media a boot prerequisite. Runtime mounting must not
duplicate FAT caches, invalidate pinned locations or hold spinlocks over I/O.

## Decision

- Publish a root-only initramfs namespace and permanent discovered-disk registry
  before EL0. Inspect MBR/FAT only for explicit requests, not configuration
  discovery. Retain primary-slot selectors `/dev/diskNpM`; no devfs is implied.
- Add `SYS_MOUNT=23` through the existing architecture-neutral syscall request
  and owned filesystem continuation. The C interface follows Linux's five
  arguments but supports only `vfat`/`fat32`, absolute existing targets, flags
  zero or `MS_RDONLY=1`, and NULL data. Return direct negative errno.
- Use bounded additive global mount publication (16 additional instances),
  short SMP locks, mount/node identity and topology revalidation. Prepare
  allocations, resolve targets and construct filesystems outside locks.
  Reject root replacement, duplicates and ancestor mounts hiding existing
  mounts. A failed request publishes no partial namespace entry.
- Reserve normalized partition identity before FAT construction. Retain the
  reservation across exact-token disk/lookup waits. Destroy unpublished state
  before releasing a failed/cancelled claim; publication permanently pins the
  successful instance. No pending continuation retains a borrowed live frame.
- Existing cwd/open descriptions retain their previous location. A fresh
  absolute traversal observes newly published mounts. There is no unmount or
  reclamation, so no reader grace period or location rewriting is needed.
- Permit every EL0 process to request global mounts until credentials and
  capabilities exist. This is an explicit experimental-system policy, not a
  security boundary or Linux privilege compatibility.
- Install a minimal `/init` and separate `/bin/shell`. Init reads the complete
  `/etc/fstab` from initramfs, validates at most 16 KiB/16 six-field entries,
  rejects duplicate normalized sources and applies automatic entries in order.
  Preserve comments, whitespace, octal escapes, defaults/rw/ro/noauto and
  zero-only dump/pass. Then exec the shell, preserving process identity.
- Missing/empty fstab requests no disk mounts. Invalid configuration or a
  required mount failure reports an error and stops initialization; no kernel
  fallback remains. Earlier successful mounts are not rolled back after a
  later failure. Ship comments-only configuration by default and separate
  storage-configured initramfs examples and test fixtures.

## Invariants

No mount lock spans traversal, allocation, block polling, usercopy, backend
callbacks or final destruction. No competing FAT instances exist for one
partition. Physical disk ownership is restored before resolving a target on
that same disk. Mounts never enter IRQ/scheduler/timer paths. Initramfs remains
the permanent readonly root and archive-backed ELF source.

## Consequences

Attaching a raw disk no longer mounts it automatically. Existing disk-root
fstab files are inert; migrate configuration into `/etc/fstab` in initramfs.
Diskless production images remain valid. Filesystem construction can fail or
take ordinary task time; short publication locks do not certify mount latency.
Device request timeouts and explicit fsync durability are unchanged.

## Alternatives considered

Keep kernel fstab parsing: preserves policy coupling. Hold a global lock over
construction: violates task/I/O invariants. Create caches before claiming a
partition: permits competing metadata owners. Rewrite existing locations:
breaks open-description/cwd lifetime. RCU/reclamation is unnecessary without
unmount. General credentials, boot-medium aliases and additional filesystems
remain separate work.

## Validation

Use host tests for publication/conflicts/rollback/pinned references, partition
claims and the userspace parser/initializer. Execute exact production init and
shell ELFs in QEMU; cover diskless startup, readonly/nested mounts, bad syscall
arguments, competing CPU requests, old cwd/FDs and two-boot explicit-fsync
persistence. Preserve VFS, process, SMP, RT and release identity gates.
Report actually executed checks separately; these are not physical SD or
power-loss guarantees.

## Related decisions

- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [ADR-0067](ADR-0067-resident-fat-and-canonical-file-handles.md)
- [ADR-0068](ADR-0068-driver-owned-storage-timeouts.md)
- [Storage guide](../../docs/storage.md)
