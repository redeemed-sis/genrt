# ADR-0001: Start with AArch64 on QEMU virt

## Status
Accepted

## Context
The project targets x86_64, ARM64, and RISC-V, but immediate multi-arch bring-up would multiply boot, interrupt, MMU, and debugging complexity.

## Decision
Use AArch64 on QEMU `virt` as the first bring-up target.
Treat multi-architecture support as a design constraint from day one, but defer second and third ports.

## Consequences
- Faster first feedback loop.
- Clear `kernel/` vs `arch/` vs `platform/` split from the beginning.
- x86_64 validation comes later, after the basic kernel loop stabilizes.
