# ADR-0004: AArch64 Boot/Exception Separation and Fatal Exception Path

## Status
Accepted

## Context
`arch/aarch64/src/boot.s` accumulated unrelated responsibilities:
- early boot/startup,
- vector table,
- exception entry,
- trap frame save/restore,
- first task enter path.

This made bring-up diagnostics and exception behavior harder to reason about.
In addition, synchronous exceptions were effectively handled by a dummy spin loop without actionable diagnostics.

## Decision
- Split early startup and exception/trap handling:
  - keep `_start` and early boot sequence in `boot.s`;
  - move vectors and trap entry/restore logic to `exceptions.s`.
- Move Rust-side exception logic to dedicated modules:
  - `exception.rs` for dispatch/diagnostics/fatal behavior and IRQ entry point;
  - `esr.rs` for minimal ESR decoding helpers.
- Replace dummy synchronous exception handling with fatal diagnostics:
  - print source/type + ESR/FAR/ELR/SPSR + key trap-frame registers;
  - stop via architecture hard-fault path.
- Introduce shared architecture hard-fault halt path (`arch_hard_fault`) and use it from both:
  - fatal exception handling,
  - kernel panic handler.

## Rationale
This keeps responsibilities explicit:
- startup flow remains simple and deterministic;
- exception flow is isolated and easier to audit;
- trap-frame ABI contract between asm and Rust is documented and checked.

## Consequences
- Boot and exception code are easier to maintain independently.
- Unhandled exceptions now produce useful early diagnostics instead of silent spins.
- Fatal paths converge on a deterministic halt behavior.
