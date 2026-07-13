# ADR-0015: AArch64 High-Half MMU Bring-Up

## Status

Accepted

## Context

The AArch64 QEMU `virt` kernel needs MMU enablement without redesigning the
existing kernel subsystems. The current boot flow, scheduler, IPC, heap, and
frame allocator already work in a low-loaded kernel image. The next milestone is
to run the main kernel at high virtual addresses while keeping the physical load
address low and preserving the rule that the generic frame allocator manages
physical frames only.

## Decision

Use a low-linked trampoline with a high-linked kernel image loaded low:

* `.boot.*` sections have low VMA and low LMA and execute before the MMU is on.
* Main kernel sections have high VMA and low LMA through linker `AT(...)`.
* The trampoline builds initial page tables, enables the MMU, switches SP to the
  high alias of the boot stack, and branches to the high `rust_entry`.
* No kernel segment is copied to another physical location.

The kernel uses a high direct-map offset:

```text
KERNEL_HVA_OFFSET = 0xffff_0000_0000_0000
HVA = PA + KERNEL_HVA_OFFSET
PA  = HVA - KERNEL_HVA_OFFSET
```

Boot page tables use TTBR0 for temporary identity mappings and TTBR1 for the
permanent high direct map. The temporary identity mapping exists only until the
high kernel has initialized memory and installed high-address MMIO users; it is
then removed by `drop_boot_identity_mapping()`.

For the AArch64 QEMU `virt` bare-metal ELF workflow, the boot protocol does not
trust `x0` as a DTB pointer. `xtask` loads a compacted QEMU-generated DTB at the
start of RAM (`0x4000_0000`), while the kernel image remains loaded at
`0x4008_0000`. A small platform-owned `.boot.text` DTB parser extracts only the
RAM, PL011, and GICv2 `reg` ranges needed to build the initial page tables
before UART or GIC are used from high virtual addresses.

The generic frame allocator remains address-agnostic. It continues to allocate
and free physical frame addresses. PA-to-HVA conversion is allowed only at
explicit dereference boundaries, such as DTB parsing, free-list metadata stored
inside physical frames, heap initialization, page-table writes, and MMIO base
aliases.

## Consequences

Positive:

* the main kernel can run in the high half with minimal `kernel/` changes
* UART, GIC, heap, scheduler, IPC, time, and thread lifecycle keep their current
  ownership model
* frame allocation semantics remain physical and suitable for later page-table
  and process work
* the low identity map can be removed after bring-up, making accidental low
  dereferences visible

Limitations:

* first-stage mappings are 2 MiB block mappings
* only TTBR1 kernel mappings are supported after boot
* no EL0, ASIDs, per-process TTBR0, demand paging, guard pages, KASLR, or page
  fault recovery yet
* mapping updates are single-core only; SMP TLB shootdown is future work
