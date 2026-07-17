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
use thread-context-only `PreemptLock` ownership. IRQs remain enabled when permitted by the
caller, while nested preemption exclusion prevents another thread from observing
allocator mutation; a requested switch is deferred until the outermost lock is
released. Boot-discovered regions and the heap range become immutable metadata
after initialization and can be read without holding the allocator lock.

## Kernel mappings

TTBR1 is the kernel address-space owner. Boot-owned tables establish the first
high mappings; runtime tables are allocated from the frame allocator and become
active before mutable VM APIs are enabled. Mapping mutation before that switch
returns `VmError::NotInitialized`, preventing lost mappings or accidental
reclaim of `.boot.bss` page tables.

The VM API exposes explicit physical/direct-map conversion, translation, and
kernel map/unmap/protect operations. Current kernel region mutation is limited
to its documented alignment and granularity.

## User address spaces

Each process owns a non-copyable `OwnedUserAddressSpace` backed by an
allocator-owned TTBR0 root. Mapping code borrows that owner. Scheduler handoff
stores only a copyable, non-owning `AddressSpaceId`, which can activate the
selected root but cannot destroy it.

User ELF segments and stacks use 4 KiB mappings with descriptor-derived user,
write, and execute permissions. ELF frames remain process-owned. Each user
thread owns one non-copyable `OwnedUserStack` containing its virtual range and
physical frames. Generic thread join/reap releases that stack before process
cleanup frees ELF frames and destroys the address-space owner. QEMU/initramfs
source bytes are not process-owned frames.

The current one-thread-per-process lifetime is:

```text
Process owns OwnedUserAddressSpace
  -> Thread borrows identity as AddressSpaceId and owns OwnedUserStack
  -> Thread exits and is reaped
  -> OwnedUserStack frames are released
  -> ELF frames and OwnedUserAddressSpace are released
```

## User copies

`memory::user` validates complete multi-page ranges against the current active
user address space and actual page permissions. Read and write validation are
separate. Copies are bounded and intended for syscall/trap context where active
TTBR0 matches the current process.

The current copy loop assumes mappings stay stable and has no exception-table
fault recovery if an actual load/store faults after validation. Future recovery
belongs inside this module, not in individual syscalls.

## Fast-path rules

- No heap allocation in IRQ, scheduler, or timed-event paths.
- Preallocate dynamic scheduler/time capacity before enabling those paths.
- Convert PA to HVA only at explicit dereference boundaries.
- Do not free boot tables through the runtime allocator.
- Keep user-copy, parsing, and frame destruction outside IRQ-disabled sections.
- Do not acquire heap or frame allocator `PreemptLock` state from IRQ context.
- No reference into mutable frame allocator state may escape its guard.

Related decisions: ADR-0007, ADR-0009 through ADR-0011, ADR-0015, ADR-0016, and
ADR-0029, ADR-0030, and ADR-0033.
