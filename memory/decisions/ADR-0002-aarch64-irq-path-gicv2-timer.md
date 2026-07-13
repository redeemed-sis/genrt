# ADR-0002: AArch64 IRQ Path via GICv2 + Architected Physical Timer

## Status

Accepted

## Context
For `Month 1 / Week 3` bring-up on QEMU `virt` (AArch64), we need a deterministic first hardware interrupt path from EL1:
- vector entry in assembly,
- interrupt controller initialization,
- timer interrupt delivery and acknowledgement.

## Decision
Use QEMU `virt` GICv2 MMIO interface:
- distributor base: `0x0800_0000`,
- CPU interface base: `0x0801_0000`.

Use architected physical timer interrupt (PPI ID 30):
- program `CNTP_TVAL_EL0` + `CNTP_CTL_EL0`,
- enable PPI 30 in GIC distributor,
- unmask IRQ in `DAIF`,
- dispatch IRQ in EL1 vector handler,
- acknowledge/EOI through `GICC_IAR`/`GICC_EOIR`.

For initial safety and observability, timer IRQ handling is one-shot:
- first timer IRQ increments an atomic counter,
- timer is disabled in handler to avoid interrupt storm during early bring-up.

## Consequences
- Provides minimal end-to-end IRQ path needed for next scheduler work.
- Keeps `unsafe` localized in arch-layer MMIO/asm code.
- Assumes QEMU `virt` GICv2 layout; non-`virt` platforms require platform-specific mapping.
