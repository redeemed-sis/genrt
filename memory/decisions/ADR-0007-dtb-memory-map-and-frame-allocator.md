# ADR-0007: DTB Memory Map and Physical Frame Allocator

## Status

Accepted

## Context
`genrt` had a working execution core, but still lacked a real physical-memory
foundation:
- no internal physical memory map,
- no explicit carving of kernel/boot-reserved RAM,
- no usable page-range view,
- no frame allocator for future heap or page-table work.

The boot path already passed `BootInfo` into the kernel and exposed the DTB
physical address, so the remaining gap was to turn those early inputs into a
small but real memory subsystem.

## Decision
- Parse DTB memory information during early boot and seed `BootInfo.memory_map`
  from it.
- For the current QEMU `virt` ELF boot workflow, generate a `virt` DTB during
  build and embed it into the AArch64 image as a fallback when firmware does
  not pass a runtime DTB pointer in `x0`.
- Keep the DTB-derived seed map small and static:
  - usable RAM regions from `/memory` nodes,
  - reserved ranges from the FDT reserve map,
  - reserved-memory child `reg` ranges when present.
- Extend `BootInfo` minimally with `dtb_size`.
- Build the real kernel memory view in `kernel::memory`:
  - collect raw RAM ranges,
  - subtract reserved ranges for:
    - kernel image,
    - boot stack,
    - DTB itself,
    - DTB-reserved regions,
  - produce page-aligned usable frame ranges.
- Implement a single-page free-list frame allocator using the free frames
  themselves to store next pointers.

## Rationale
This keeps the design aligned with current bring-up constraints:
- no heap allocation,
- no MMU dependency,
- no generic callback or VM framework,
- explicit and auditable physical memory bookkeeping.

It also establishes a direct foundation for:
- kernel heap backing,
- page-table allocation,
- mailbox or buffer backing storage,
- future memory-ownership structures.

## Consequences
- `BootInfo` remains lightweight, but now carries a DTB-sized memory-map seed.
- The AArch64/QEMU build flow now has an explicit embedded-DTB fallback so the
  kernel still reasons from a DTB-derived memory map even when `-kernel` boot
  does not hand off `x0`.
- `kernel::memory` becomes the owner of:
  - physical memory bookkeeping,
  - usable page-range derivation,
  - physical frame allocation.
- The allocator is intentionally simple:
  - single-core only,
  - free-list only,
  - one frame at a time.
- MMU and heap allocation remain separate follow-up milestones.
