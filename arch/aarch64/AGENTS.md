# AArch64 instructions

- Treat all code reachable before `SCTLR_EL1.M` is enabled as a low-linked
  closed world in `.boot.text`, `.boot.rodata`, and `.boot.bss`.
- Pre-MMU code must not acquire compiler runtime, panic, formatting, logging,
  high-linked helper, `memcpy`, or `memset` dependencies.
- Keep Rust trap-frame definitions, assembly save/restore offsets, SPSR mode
  semantics, and stack ownership synchronized as one ABI.
- Preserve the EL1 task-call versus EL0 syscall distinction.
- Derive GIC, timer, UART, RAM, and DTB behavior from documented architecture
  data and the controlled QEMU platform configuration; never infer addresses.
- Localize MMIO and system-register `unsafe` operations and document ordering,
  alignment, ownership, and active translation-table assumptions.
- Every boot/linker change must pass the xtask post-link `.boot.text` autonomy
  check. Exception, IRQ, MMU, or scheduler-return changes require the relevant
  QEMU contract and normally `cargo xtask ci`.
