# ADR-0067: Resident FAT and canonical file handles

## Status

Accepted

The FAT-wide call/time budget is superseded by
[ADR-0068](ADR-0068-driver-owned-storage-timeouts.md). Driver deadlines and
actual device/metadata failure latching remain accepted.

Partially supersedes [ADR-0064](ADR-0064-qemu-block-storage-and-fat32.md)
for library selection, handle lifetime, metadata publication and FAT budget
accounting. Relaxes [ADR-0063](ADR-0063-reusable-open-description-io-storage.md)
only for FAT-library cache/run metadata allocations. Generic reusable request
storage, transport ownership and the syscall ABI are unchanged.

## Context

Reconstructing a filesystem and file for every operation repeats FAT and path
reads. The retained-handle experiment confirmed that cached file access avoids
those reads, but independent library file objects can overwrite newer lengths
with stale metadata. The user permits small FAT-library allocations in exchange
for a persistent filesystem and a simpler public handle/sync API.

## Decision

- Pin `resident-fat` 0.3.1 without optional features. Retain one owning
  `FileSystem<Disk>` across operations. Keep `starry-fatfs` only as a host-test
  formatter and in the isolated comparison PoC, not in production dependencies.
- Each stable node owns either a directory cluster or one canonical resident
  `File`. All descriptions use that same file object for read/write/truncate,
  stat and EOF selection. OFD positions remain separate; fork still shares
  descriptions. Long-name, short-name and case aliases converge through the
  library's canonical directory entry. Local paths are retained for discovery
  and creation, not reconstructed for payload I/O.
- Transfer the boxed owner using the existing logical lease. No spinlock spans
  library calls, allocation, device waits, usercopy or destruction. Append holds
  the lease across EOF selection and every fragment of the complete write.
- The pinned library erases its clock as `Box<dyn Clock>` without `Send`.
  Permit one localized `unsafe impl Send` for the private volume owner: only
  the default stateless `EpochClock` is installed, no custom-clock setter or
  filesystem reference escapes, and all other owned members are transferable.
  Do not implement `Sync` for the filesystem. Re-audit this on version changes.
- Preserve OFD-owned staging with a 4096-byte baseline and fallible retained
  growth. Permit library-internal run/cache metadata allocation in ordinary
  IRQ-enabled task context. This exception does not apply to generic request
  construction, the block driver, IRQ, scheduler or timer paths. FAT residency
  and parsed directory caches consume additional heap; they are not an RT
  allocation-free or fixed-memory guarantee. Library-reported allocation
  failures become `ENOMEM`.
- `fsync` calls library `sync()` to publish FAT/FSInfo and its dirty-state
  metadata, then explicitly flushes the block device. It does not unmount or
  invalidate canonical files. Unsynced volume drop and final OFD close perform
  no disk I/O. Ordinary operations do not imply durability.
- The library does not return progress with failed read/write calls. Split
  payload calls at sector boundaries and return only completed fragments. A
  failing fragment may still have changed data before metadata failed; its
  bytes are not claimed as successful and it is never retried internally.
  Latch transport, admission-budget and inconsistent-metadata failures before
  returning ownership. No later operation or fsync may hide the failure.
- Keep checked BPB preflight, partition bounds and existing node/path limits.
  Reject mirroring-disabled/active-FAT volumes with `ENOTSUP` before mount:
  the pinned library reads FAT 0 and writes all copies, ignoring active-table
  selection. A stale first copy must not be mistaken for the active table.
  A task operation admits at most 32768 device calls/library checkpoints over
  two counter seconds, including checkpoints between payload fragments and
  after cached work. A running library call is not preemptively interrupted;
  CPU-only FAT scans and cached traversal are not independently timed. Device
  requests retain their own deadlines. This is not a hard latency bound.

## Invariants

One canonical file owns current size/cluster metadata for all its OFDs. Never
store independently mutable copies in descriptions or reopen files after each
payload operation. Preserve exact-token waiting, partition ownership, explicit
durability and non-I/O close. No FAT work or allocations enter RT fast paths.

## Consequences

FAT and directory data remain resident, avoiding repeated mount/path reads.
Warm payload operations still may allocate small library vectors. Large or
fragmented volumes consume more memory; library internals do not provide a
fixed heap bound. Sector-fragment writes trade bulk throughput for confirmed
partial-progress accounting. No power-loss recovery or transaction guarantee
is added, and no physical Pi storage support follows from this migration.

## Alternatives considered

Keep leased reconstruction: simple safe lifetime handling, but repeated reads.
Retain starry handles: requires dependent-owner handling and a separate policy
for filesystem metadata sync. Broader library or VFS redesign is unnecessary
for the accepted allocation exception.

## Validation

Host tests cover shared canonical metadata, independent positions, case aliases,
absence of remount reads, gap zeroing, append/truncate, FSInfo, partial read and
write faults, metadata failure after payload, failure latching, readonly media,
geometry, admission budgets and non-I/O close. A regression rejects differing
FAT copies with FAT 1 active before any device write. Generic VFS allocation
tests remain unchanged. Four-CPU `storage-contract` verifies two boots of one image,
concurrent append and shared fork offsets, with independent mtools extraction
and fsck checks. These checks establish functionality, not target performance.

## Related decisions

- [ADR-0062](ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md)
- [ADR-0065](ADR-0065-fsync-syscall.md)
- [ADR-0066](ADR-0066-usercopy-length-and-owned-io-growth.md)
- [Storage guide](../../docs/storage.md)
- [Library comparison](../../tests/storage/fat-poc/README.md)
- [Pinned library source](https://docs.rs/crate/resident-fat/0.3.1/source/)
