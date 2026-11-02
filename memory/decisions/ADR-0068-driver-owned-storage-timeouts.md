# ADR-0068: Driver-owned storage timeouts

## Status

Accepted

Supersedes the FAT-wide call/time budget in
[ADR-0067](ADR-0067-resident-fat-and-canonical-file-handles.md).
Persistent handles, partial-progress accounting and ownership remain unchanged.

## Context

A fixed call count and two-second deadline can reject valid large or slow FAT
operations. They do not interrupt CPU-only library work and therefore do not
provide a filesystem latency bound. Exhausting this arbitrary budget must not
permanently disable an otherwise functioning volume.

## Decision

- Remove FAT-wide counters, deadlines and checkpoints from the sector adapter
  and operation paths. No timer reads are needed to admit FAT work.
- Keep request/transfer deadlines in the block driver, including timeout
  poisoning and retention of uncertain DMA resources. Keep sector alignment,
  capacity and per-transfer size validation.
- Retain device-error and inconsistent-metadata latching, confirmed partial
  progress, explicit filesystem/device sync and non-I/O close. Removing the
  budget does not enable retries after ambiguous failures.
- FAT traversal and corruption checks remain library responsibilities. A
  complete FAT call has no hard wall-time guarantee and stays outside IRQ,
  scheduler, timer-dispatch and RT measurement paths.

## Invariants

No allocation or disk waits enter shared owner locks or RT paths. Device
timeouts remain errors, not cancellation. Correct repeated I/O is not rejected
solely because of the number or duration of preceding steps in a FAT operation.

## Consequences

Large valid operations can continue while individual device transfers succeed.
The logical volume lease can remain occupied for their full duration; this is
not a fairness or worst-case latency guarantee.

## Alternatives considered

Increasing the constants only moves the arbitrary failure threshold. Keeping
the deadline without poisoning still rejects legitimate operations without
bounding an already-running library call.

## Validation

Host tests perform 40000 reads without resetting adapter state, check invalid
transfer rejection without poisoning, and verify that actual device timeout
and I/O errors prevent read/write/sync resubmission. Existing FAT persistence,
partial-error and four-CPU QEMU storage contracts remain applicable.

## Related decisions

- [ADR-0064](ADR-0064-qemu-block-storage-and-fat32.md)
- [Storage guide](../../docs/storage.md)
