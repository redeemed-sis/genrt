# Memory subsystem

The generic memory layer owns physical-resource policy and architecture-neutral
VM contracts. AArch64 page-table encodings and register operations remain in
`arch/aarch64`.

## Bootstrap and physical map

`memory::init` consumes `BootInfo`, normalizes DTB RAM ranges, and carves out the
kernel image, boot stack, DTB, and initramfs loader window. Fixed-capacity arrays
bound early range processing. The frame allocator manages page-aligned physical
frames and stores free-list metadata through explicit high direct-map aliases.

The bootstrap heap is one contiguous 16 MiB frame range. Once allocated, it is
removed from the frame free list and initialized through its HVA. Allocation is
allowed during bootstrap and thread context. The heap and runtime frame free list
use SMP `SpinLock` ownership. IRQs remain enabled when permitted by the caller,
while nested preemption exclusion prevents a holder from being switched away; a
requested switch is deferred until release. Frame ownership is removed under
that lock, then zeroing/copying happens after release. Boot-discovered regions
and the heap range use Release/Acquire immutable publication and can be read
without holding the allocator lock.

## Kernel mappings

TTBR1 is the kernel address-space owner. Boot-owned tables establish the first
high mappings; runtime tables are allocated from the frame allocator and become
active before mutable VM APIs are enabled. Mapping mutation before that switch
returns `VmError::NotInitialized`, preventing lost mappings or accidental
reclaim of `.boot.bss` page tables.

The VM API exposes explicit physical/direct-map conversion, translation, and
kernel map/unmap/protect operations. The generic VM facade serializes TTBR1
writers and may nest only into the frame allocator. AArch64 keeps descriptor
updates and the barrier/TLBI sequence architecture-local, uses inner-shareable
broadcast invalidation, and only reclaims table frames after invalidation.

## User address spaces

Address-space construction begins as a non-copyable
`StagedUserAddressSpace`. ELF and stack mapping APIs accept only this unpublished
mutable stage. Assigning a future logical CPU consumes it and yields the
process-owned `OwnedUserAddressSpace`; published roots are immutable. Scheduler
handoff stores only a copyable, non-owning `AddressSpaceId` containing the same
owner, which can activate the root but cannot map or destroy it.

User ELF segments and stacks use 4 KiB mappings with descriptor-derived user,
write, and execute permissions. ELF frames remain process-owned. Each user
thread owns one non-copyable `OwnedUserStack` containing its virtual range and
physical frames. Generic thread join/reap releases that stack before process
cleanup frees ELF frames and destroys the address-space owner. QEMU/initramfs
source bytes are not process-owned frames.

The current one-thread-per-process lifetime is:

```text
Process owns OwnedUserAddressSpace
  -> Process, AddressSpaceId, and main Thread agree on one CPU owner
  -> Thread borrows identity as AddressSpaceId and owns OwnedUserStack
  -> Thread exits and is reaped
  -> OwnedUserStack frames are released
  -> ELF frames and OwnedUserAddressSpace are released
```

Only the immutable owner CPU may activate a published TTBR0 root. The generic
VM layer tracks each CPU's active root and rejects destruction while any CPU
still records it. AArch64 performs local invalidation on TTBR0 activation and
clear only when the actual root changes; staged mappings need no invalidation
because they have never been active. Permanent idle retains an immutable user
root while executing only trusted high-half code, with no user-pointer access.
The active record remains published during idle. Other kernel threads clear
the root; user exit detaches before a retired join/reap can reclaim frames.
Runtime TTBR1 mappings remain shared, mutable, and use their existing
inner-shareable broadcast invalidation protocol. See
[ADR-0054](../../../memory/decisions/ADR-0054-idle-user-root-retention-and-detach.md).

## User copies

`memory::user` validates complete multi-page ranges against the current active
user address space and actual page permissions. Read and write validation are
separate. Copies are bounded and intended for syscall/trap context where active
TTBR0 matches the current process.

The executing CPU's VM publication supplies the root identity in O(1), without
locking or looking up the scheduler thread. A user-access eligibility flag is
published only after successful activation and revoked before kernel/idle
execution or detach. Idle's retained physical root continues protecting its
lifetime but does not authorize usercopy. IDs are borrowed only during a
stable syscall context, never retained across handoff. Per-CPU publication uses
fixed, separately aligned storage. See
[ADR-0058](../../../memory/decisions/ADR-0058-bounded-sleep-and-vm-owned-usercopy.md).

Validation finishes before any destination byte changes. Single-page reads
are validated through the current hardware EL0 translation;
AArch64 uses AT S1E0R and preserves PAR under local IRQ exclusion. Generic
range checks and VM current-user eligibility remain mandatory. Cross-page
reads and all writes retain software mapping/permission walks. No validation
result is cached across roots or context handoff. See
[ADR-0059](../../../memory/decisions/ADR-0059-native-counter-sleep-and-active-read-validation.md).

Fixed-length copies use volatile, naturally aligned machine-word accesses to
user memory with byte prefixes and tails; kernel buffers need not be
word-aligned. No access extends past the validated range, and aligned words
cannot straddle a 4 KiB page.
Bounded C-string scans remain bytewise so finding a NUL never requires reading
a following page. `getcwd` uses the word-copy writer plus a separate NUL.

The current copy loop assumes mappings stay stable and has no exception-table
fault recovery if an actual load/store faults after validation. Future recovery
belongs inside this module, not in individual syscalls.

## Fast-path rules

- No heap allocation in IRQ, scheduler, or timed-event paths.
- Preallocate dynamic scheduler/time capacity before enabling those paths.
- Convert PA to HVA only at explicit dereference boundaries.
- Do not free boot tables through the runtime allocator.
- Keep user-copy, parsing, and frame destruction outside IRQ-disabled sections.
- Do not acquire heap or frame allocator `SpinLock` state from IRQ context.
- No reference into mutable frame allocator state may escape its guard.

Related decisions: ADR-0007, ADR-0009 through ADR-0011, ADR-0015, ADR-0016, and
ADR-0029, ADR-0030, and ADR-0033.
