# ADR-0016: First AArch64 EL0 Process Bring-Up

## Status
Accepted

## Context
The kernel already has high-half EL1 execution, runtime-owned TTBR1 page tables,
preemptive IRQ-return scheduling, kernel thread lifecycle, and a trap-frame ABI
that can restore either EL1 or EL0 frames. The next milestone needs a first EL0
program without introducing VFS, ASIDs, or per-process scheduler redesign.

## Decision
Use a narrow AArch64/QEMU bring-up path:

- `xtask` builds a freestanding AArch64 ELF user payload and loads that ELF file
  as raw bytes with QEMU generic loader at a fixed physical address.
- The kernel parses the ELF image and maps its `PT_LOAD` segments into the
  process TTBR0 address space.
- The fixed loader region is reserved from the physical frame allocator.
- A kernel init thread, running after scheduler entry, creates the first user
  address space and user thread.
- User address spaces are allocator-owned TTBR0 roots with 4 KiB user page
  mappings. TTBR1 remains the runtime kernel direct map.
- Scheduler handoff activates TTBR0 for user threads and clears TTBR0 for kernel
  threads.
- Lower-EL `svc #0` is the user syscall ABI. Current-EL `svc #0` remains the
  kernel task-call ABI.
- The initial syscalls are `write(fd=1, ptr, len)` and `exit(code)`.

## Invariants
- The generic frame allocator remains physical-address based and MMU-agnostic.
- Page-table entries contain physical addresses, never high virtual aliases.
- PA to HVA conversion happens only at dereference boundaries.
- Scheduler fast paths do not allocate; TTBR0 activation only writes system
  registers and invalidates TLBs.
- User mappings are non-global and EL0-accessible; ELF RX segments are not
  writable, RW segments are non-executable, and user stack is non-executable.
- The initial `copy_from_user` path is bring-up-only: it validates the user VA
  range and present TTBR0 mappings, then copies through the active user mapping.

## Consequences
- The first EL0 C ELF program can print through `sys_write` and terminate through
  `sys_exit`.
- The kernel can join the user main thread through existing thread lifecycle
  semantics.
- The design intentionally leaks no userspace policy into the generic frame
  allocator, but the process model is still intentionally minimal.
- Future work should replace the fixed QEMU loader reservation with generated
  ELF/initramfs metadata, add ASIDs, fault-aware `copy_from_user`, and richer
  wait/exit process semantics.
