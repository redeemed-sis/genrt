# ADR-0063: Reusable open-description I/O storage

## Status

Accepted

Extends [ADR-0062](ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md)
with an allocation policy for ordinary read/write requests. Namespace,
descriptor sharing, exact waits, and syscall ABI remain unchanged.

## Context

The initial VFS allocated a staging vector and boxed syscall continuation for
each I/O call. Moving a vector into a request did not copy its bytes, but the
per-call heap work was unnecessary. A shared open description already provides
exclusive logical ownership across backend work, userspace copy, and waits.

## Decision

- Provision one reusable backend request and at least 4096 bytes of staging
  when opening an OFD. A transaction leases this storage; it does not allocate
  a second request or staging buffer. Fork shares the OFD and its storage;
  independent opens provision independent storage.
- Acquire the logical lease before copying write input. A competing caller
  retains only small operation metadata and an exact wait, without accessing
  the active request. Copy input once, before backend submission, and retain
  that state across retries. Zero-length calls do not wait for the lease.
- Keep pending syscall metadata inline in process-owned storage, not in a
  newly allocated box per call. Payload bytes remain in heap storage provisioned
  at open, rather than a pathname- or page-sized kernel-stack object.
- Keep staging capacity after completion or error. Grow only when the required
  size exceeds both 4096 bytes and the retained capacity. Never shrink storage
  between calls. Internal larger-request support does not raise the existing
  1024-byte userspace-copy limit; syscall transfers remain partial and bounded.
- Backend read/write paths must not allocate ephemeral formatting, request, or
  continuation objects. Prepare backend-private I/O state during open. Generated
  reads use bounded existing or stack storage. The test backend provisions
  4096 bytes of mutable file backing at creation; backing growth beyond that
  capacity is explicit, separate from transient per-call storage.
- Reset operation-specific state and return the reusable storage before waking
  competing callers. The backend reset hook clears progress without allocating
  or releasing reusable read/write resources. Preserve backend open state and
  commit offsets only after successful usercopy. Backend callbacks, copying,
  growth, and destruction run
  outside process/scheduler/OFD locks in finite task context.

## Invariants

- A small read/write uses no heap allocation or release for transient I/O state.
- One OFD has at most one leased request; a transaction retains an OFD reference
  without creating an OFD-to-transaction reference cycle.
- Pending retries neither recopy write input nor repeat submitted side effects.
- Failed write input copies submit nothing; failed output copies do not advance
  positions. Both return reusable storage for the next caller.
- Final close and process cleanup destroy storage only outside owner locks.

## Consequences

Open costs more memory and setup work in exchange for allocation-free ordinary
I/O. At most 116 distinct ordinary OFDs across four full process tables consume
464 KiB of baseline staging, excluding request and backend metadata. Shared
inherited descriptions do not multiply that storage. This is not a latency
guarantee or a change to syscall transfer sizes.

## Alternatives considered

- Per-call vectors and boxed continuations: unnecessary allocator traffic.
- A per-process buffer: does not provide the requested OFD-owned storage.
- Inline 4096-byte transaction payloads: increase stack and process-state moves.
- Holding an OFD spinlock throughout I/O: prevents safe blocking.

## Validation

Host tests count current-thread allocation, reallocation, and release after
open/setup, check storage reuse and growth, and exercise error and pending
paths. Existing shared-offset, append, cleanup, and QEMU VFS contracts remain
functional regression checks. Type-size checks constrain inline continuation
metadata; payload capacity is not part of that inline footprint.

## Related decisions

- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [ADR-0062](ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md)
- [Filesystem guide](../../kernel/src/fs/README.md)
