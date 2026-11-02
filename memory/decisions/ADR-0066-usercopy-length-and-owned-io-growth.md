# ADR-0066: Usercopy length and owned I/O growth

## Status

Accepted

Supersedes only the 1024-byte transfer cap in ADR-0023 and ADR-0063. Their
descriptor, directory-record, shared-position and reusable-storage policies
remain in force.

## Context

The bring-up `MAX_USER_COPY = 1024` unnecessarily splits normal filesystem I/O
and was inherited by unrelated bounded service buffers. Usercopy already checks
overflow, the allowed user interval, every page and access permission before
copying; a global byte cap is not a substitute for these checks.

## Decision

Remove the global copy cap. Copy helpers process their supplied slice length,
allocating nothing, validating the entire range before any destination write.
Current immutable address-space ownership remains required; fault-fixup tables
are not introduced. Potentially large validation/copy runs in ordinary task
context with IRQs enabled and no owner locks held.

Filesystem read/write retain the requested count, rejecting counts above
`isize::MAX` because successful byte counts must fit the ABI. One OFD-owned
buffer starts at 4096 bytes and grows fallibly only when a larger request needs
it; retain capacity after completion/error. Allocation failure is `ENOMEM`, not
a hidden short transfer cap. Actual backends may still return partial progress
or EOF. No second staging layer or request-sized stack array is added.

Directory calls emit one naturally bounded dirent and need only that record's
staging. Stdin returns currently available bytes from its fixed RX ring.
Console output validates the full request then streams through fixed stack
chunks, returning only accepted bytes or `EAGAIN` if the TX queue accepts none.
Diagnostic logging retains its explicit drop-on-full policy. Frozen profiling reads
retain their own 4096-byte staging limit to bound IRQ-masked leaf-lock work;
that service limit is independent of usercopy.

## Invariants

Failed full-range validation does not copy a prefix. Failed file-output copy
does not commit the shared offset. Invalid write input submits no backend
mutation. Small I/O allocates nothing after open; only retained storage grows.
No large copy runs in scheduler/IRQ/timer paths or beneath shared owner locks.

## Consequences

Large requests can consume more of the fixed heap and take proportionally more
task time. This is not an RT latency bound or a promise that every request
completes fully. Path limits, protocol/diagnostic budgets and block/FAT operation
budgets remain distinct resource contracts.

## Alternatives considered

Raising a global cap to 4096 or 65536 preserves the same unnecessary ABI limit.
Growing stack arrays violates bounded kernel-stack ownership. Fresh heap buffers
per call violate the reusable I/O policy.

## Validation

Host copy loops exercise aligned/unaligned 64 KiB ranges and guard bytes. The
four-CPU VFS contract requires a 65573-byte read/write in one call on its
controlled backend, checks later-page invalid ranges without offset consumption,
and rejects unrepresentable counts. FAT and diskless contracts retain their
normal partial-I/O/error semantics. Repeated large-buffer reuse and failed
growth remain allocator-instrumented host tests.

## Related decisions

- [ADR-0023](ADR-0023-directory-fds-and-getdents64.md)
- [ADR-0063](ADR-0063-reusable-open-description-io-storage.md)
- [ADR-0064](ADR-0064-qemu-block-storage-and-fat32.md)
