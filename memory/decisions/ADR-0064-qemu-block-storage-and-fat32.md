# ADR-0064: QEMU block storage and FAT32 ownership

## Status

Accepted

Library selection, temporary-handle reconstruction, FAT allocation policy and
budget accounting are superseded by
[ADR-0067](ADR-0067-resident-fat-and-canonical-file-handles.md). The block,
platform and workflow decisions remain accepted. Single-disk staging and
unconditional first-partition mounting are superseded by
[ADR-0070](ADR-0070-fstab-boot-mounts-and-shared-disks.md).

Extends ADR-0062 and ADR-0063. The ramfs root, immutable namespace and shared
open-description model remain unchanged.

## Context

[Issue #24](https://github.com/redeemed-sis/genrt/issues/24) needs persistent
QEMU storage without putting disk work on scheduler or interrupt paths. FAT
library handles cache directory metadata; merely serializing independent stale
handles can still lose file length after append/truncate.

## Decision

- Generic `BlockDevice` owns 512-byte sector I/O, capacity, readonly status and
  explicit sync. Calls validate alignment, range and a 128-sector maximum.
  MBR validation checks all four primary extents before selecting the first
  FAT32 type 0x0b/0x0c. Partition-relative translation cannot escape its extent.
- AArch64 discovers QEMU virtio-mmio resources and GICv2 SPIs from DTB. Use
  pinned `virtio-drivers` 0.13.0 transport and split-queue primitives, without
  indirect/event-index/packed features. One queue of eight descriptors and one
  permanent DMA request page serve one outstanding request. Caller buffers
  never become DMA storage. QEMU DMA coherence is platform-specific.
- Completion polling is bounded ordinary-thread device work, with IRQs enabled
  and no shared lock retained: one second per request, five per transfer.
  Storage does not enable its SPI or add an IRQ handler. A timeout/uncertain
  completion poisons the device and retains potentially device-owned DMA pages;
  timeout is not cancellation. No disk work belongs in an RT measured interval.
- Build the namespace in the first initialization thread, before first process
  creation, not in early IRQ-masked `kernel_main`. Discover and mount optional
  storage there, then publish once. Test coordinators that spawn EL0 initialize
  the same namespace. Pi storage remains unsupported and diskless is normal.
- Choose pinned `starry-fatfs` 0.4.1-preview.2 with defaults and `alloc` disabled,
  `lfn` and `unicode` enabled. The crate's fixed LFN buffer permits reconstructing
  temporary handles without heap work. No crate fork or unsafe lifetime/Send
  extension is needed.
- A FAT instance serializes the whole operation with an exclusive logical volume
  lease, not a held spinlock. Stable node IDs reference canonical paths. Each
  operation mounts the borrowed partition, opens a fresh handle, seeks using
  the VFS-owned position, explicitly flushes/drops handles, then consumes
  unmount. Thus subsequent opens see current entry/cluster/FSInfo metadata.
  Append selects current EOF under that same lease. No I/O occurs on OFD close.
- Explicit sync runs after unmount and includes the block-device durability
  operation. Library file flush/unmount alone does not guarantee device flush.
  Preserve exact successful prefixes on write failure, never replay them. A
  failed device operation or unmount latches volume failure rather than allowing
  later sync to claim success. This is not crash consistency or atomic writes.
- A complete FAT operation admits at most 32768 byte-I/O/seek calls and two
  counter seconds before admission of another call; a submitted device call
  retains its own timeout. Cleanup shares the same budget. Nodes are limited
  to 4096, FAT-local paths to 1024 bytes/32 components. These are explicit
  storage-resource limits, not generic usercopy limits.
- Preflight checked BPB geometry before the library's native-width arithmetic.
  Keep operation stack frames separate and reconstruct directory paths one
  component at a time; never use library path recursion on a bounded kernel
  stack. Test the full accepted component depth in debug/instrumented QEMU.
- Canonical xtask `--disk` attaches an existing regular raw file read-write.
  JSON blockdev arguments prevent filename option injection. DTB generation and
  launch share the attachment, with distinct disk-bearing DTB names. Never
  create, format or repartition user images implicitly. Test-only formatting
  operates exclusively on a newly created owned fixture.

## Invariants

No allocation, FAT traversal, device waits, usercopy or destruction in IRQ,
scheduler, timer-dispatch or held owner-lock paths. DMA ownership outlives every
submission, including uncertain failures. Small read/write has no temporary
allocation; namespace operations may allocate bounded metadata in task context.
An exclusive volume lease covers metadata reconstruction and publication.

## Consequences

Repeated mount/path/chain reads cost storage traffic, especially for fragmented
large files. Simplicity and safe ownership take priority over caching in this
first implementation. Existing MBR images are supported; GPT, extended
partitions, Pi SDHOST, dynamic unmount, page caching and power-loss recovery are
not delivered here. FAT-local paths require UTF-8 names; non-ASCII legacy OEM
short names without an LFN are explicitly unsupported.

## Alternatives considered

The [focused executable PoC](../../tests/storage/fat-poc/README.md) compared the
published libraries on the same 64 MiB FAT32 volume:

| Observation | resident-fat 0.3.1 | starry-fatfs .2, no alloc |
| --- | ---: | ---: |
| Retained heap after mount | 517105 bytes | 0 |
| Warm small overwrite | 2 allocations/call | 0 |
| Incremental 2 MiB write | 12311 allocations | 0 |
| LFN mount/open/I/O/unmount cycle | Not measured | 0 allocations/frees/reallocations |
| Stale independent writers | Length loss | Length loss |
| Injected partial write | Error hides changed prefix | Individual calls expose progress |

Resident's cache/metadata allocation conflicts with the small-I/O policy.
Persistent starry handles would require a different authoritative-handle owner
and public filesystem-sync exposure; short-lived leased reconstruction avoids
both changes. Handwritten FAT and vendoring the full library are unnecessary.
These measurements establish feasibility, not target latency.

## Validation

Host tests cover block bounds/MBR, real queue/discovery logic, DMA/error/timeout
lifetime, FAT multi-cluster I/O, allocation counts, readonly media, partial
errors, sync failure and operation budgets. `storage-contract` boots the real
production kernel on four QEMU CPUs twice using one raw image. It checks normal
VFS operations, fork offsets, concurrent append, explicit sync and persistent
payload bytes. Host mtools extraction/checksum and fsck run between/after boots.
Diskless contracts and release composition remain separate gates. No physical
Pi-storage or RT-latency claim follows from these storage tests.

## Related decisions

- [ADR-0062](ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md)
- [ADR-0063](ADR-0063-reusable-open-description-io-storage.md)
- [ADR-0065](ADR-0065-fsync-syscall.md)
- [Published virtio transport/queue](https://docs.rs/virtio-drivers/0.13.0/virtio_drivers/)
- [Published starry-fatfs source](https://docs.rs/crate/starry-fatfs/0.4.1-preview.2/source/)
- [Published resident-fat source](https://docs.rs/crate/resident-fat/0.3.1/source/)
