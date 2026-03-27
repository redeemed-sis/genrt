# Architecture notes

Initial platform sequence:
1. AArch64 on QEMU `virt`
2. x86_64 on QEMU `q35`
3. RISC-V on QEMU `virt`

Layering rule:
- `kernel/` contains architecture-neutral logic
- `arch/` contains CPU-specific code
- `platform/` contains machine-specific code
