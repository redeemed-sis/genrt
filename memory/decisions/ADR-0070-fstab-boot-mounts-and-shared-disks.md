# ADR-0070: Fstab boot mounts and shared physical disks

## Status

Accepted

Partially superseded by [ADR-0072](ADR-0072-runtime-mounts-and-userspace-init.md)
for disk-root fstab, fallback placement and pre-EL0 mount policy. The shared
physical-disk reservation remains in force.

Partially supersedes [ADR-0064](ADR-0064-qemu-block-storage-and-fat32.md)
for single-disk staging, exclusive parent consumption by the mounted partition,
and unconditional `/mnt/disk` placement. The generic block interface, partition
bounds, driver deadlines and optional diskless boot remain unchanged.

## Context

The single boot-device slot and immediate first-partition mount combine driver
discovery with namespace policy. Configuring multiple partitions requires both
an explicit boot policy and serialization of their shared physical controller.
Holding a spinlock over disk I/O or sleeping inside a backend poll violates the
existing task-window and exact-wait contracts.

## Decision

- Probe supported block devices in firmware order and retain their exclusive
  owners in bounded bootstrap staging. Device discovery does not choose mount
  paths or inspect filesystem content.
- Use the first disk and its first primary MBR FAT32 partition as the boot
  configuration source. Read `/fstab` at that filesystem's root. Only absence
  selects the fallback mount at `/mnt/disk`; malformed configuration or media
  errors fail initialization. An empty present file requests no mounts.
- Parse at most 16 KiB and 16 six-field entries before applying them. Sources
  use `/dev/diskNpM`, with discovery-order disks starting at zero and original
  MBR slots starting at one. These are selectors, not a devfs ABI. Support
  `vfat`/`fat32`, `defaults`/`rw`/`ro`/`noauto`, and zero dump/pass fields.
  Apply automatic entries in file order at existing directory mountpoints.
  Do not replace the initramfs root or silently ignore unsupported fields.
- Reject duplicate sources before constructing competing FAT
  instances. Reuse the configuration-source filesystem if the table mounts it.
  Readonly policy rejects mutation before backend side effects; explicit sync
  must not publish filesystem metadata for a readonly mount.
- Share a physical disk through an exclusive logical reservation. A FAT
  operation acquires both its volume and the physical device before library
  work. Contention registers an exact token and returns `Pending`, without
  issuing an I/O operation. Sector calls use the transferred device directly.
  Short spinlocks only move ownership and waiter registrations; no spinlock
  spans library work, allocation, disk polling or destruction. Return owners
  before completing waiters outside locks, including all failure paths.
- Keep namespace construction in the ordinary initialization task before EL0
  and global publication. No syscall, disk format or usercopy ABI changes.

## Invariants

One physical driver has one active owner. Every mounted partition stays within
its validated MBR extent and has one authoritative FAT instance. No storage work
enters IRQ, scheduler or timer dispatch. Wait-before-commit notifications retain
the existing generation/sequence identity. Driver poison and uncertain DMA
backing survive transfer between partition users.

## Consequences

Existing images without `fstab` retain `/mnt/disk`. Configured images can mount
multiple partitions without transferring the physical controller permanently
to the first one. Partition operations on the same disk serialize; different
disks have independent owners. Configured mountpoints must already exist and
nested mounts require parent-first ordering. Initialization failure does not
promise rollback of device or filesystem side effects.

GPT, extended partitions, UUID/LABEL selectors, devfs, hotplug, dynamic unmount,
root replacement, automatic fsck and physical Pi storage remain separate work.

## Alternatives considered

Always mount the first partition: cannot express the requested policy.
Hold a disk spinlock across I/O: violates ownership and scheduling invariants.
Block inside the FAT sector adapter: invalid beneath a live syscall task window.
Duplicate physical owners or filesystem caches: violates device exclusivity and
canonical metadata ownership. A general device graph is unnecessary.

## Validation

Use focused host checks for parsing, boot selection, ownership restoration,
partition isolation and readonly behavior. Exercise absent-fstab fallback and
configured same-disk mounts through the real QEMU storage contract, including
two-boot persistence and independent host extraction/fsck. Run normal diskless
contracts as regressions. These are functional checks, not hardware timing or
power-loss guarantees; report executed gates separately.

## Related decisions

- [ADR-0062](ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md)
- [ADR-0067](ADR-0067-resident-fat-and-canonical-file-handles.md)
- [ADR-0068](ADR-0068-driver-owned-storage-timeouts.md)
- [ADR-0069](ADR-0069-shared-runtime-dtb-and-staged-probe.md)
- [Storage guide](../../docs/storage.md)
