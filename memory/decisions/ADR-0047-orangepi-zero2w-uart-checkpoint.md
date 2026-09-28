# ADR-0047: Isolated Orange Pi Zero 2W UART checkpoint

## Status

Superseded for active Orange Pi support by
[ADR-0052](ADR-0052-retire-hardware-uart-checkpoint.md). This historical
decision did not supersede QEMU runtime boot decisions.

Partially superseded by [ADR-0048](ADR-0048-orangepi-shared-boot-and-fit.md)
for the independent entry/linker, standalone-only composition, and legacy
uImage/manual-DTB packaging. Historical hardware evidence remains unchanged.

## Context

Issue #20 needs a repeatable UART-loaded first instruction/output checkpoint
on H618 without rewriting the board's storage. The observed firmware is vendor
BOOT0 and BL31 followed by AArch32 U-Boot 2018.05, loaded from SPI NOR. Its
`loady` and `bootm` commands work, but `booti` is absent and `go` cannot directly
enter an AArch64 payload. Generic kernel runtime initialization would introduce
unverified assumptions about UART, interrupt, memory, and firmware interfaces.

## Decision

Add an opt-in `cargo xtask build-orangepi-zero2w` workflow for an independent
assembly/linker image under `arch/aarch64/platform/orangepi-zero2w`. Keep the
existing QEMU build, artifact identity, runtime kernel, and release unchanged.

Upload a deterministic legacy uImage to `0x41000000`. Its load/entry address
is `0x42000000`, with a linker-bounded 64 KiB memory window. Use the vendor's
ARM/Linux/kernel tag compatibility check while placing AArch64 instructions
inside the wrapper. U-Boot/BL31, not genrt, own the AArch32-to-AArch64 switch.

The entry records original x0..x3 and CPU state, checks the translation/cache
precondition, establishes private stack storage, programs hardcoded H618
UART0, prints a marker and diagnostic observations, drains TX, and parks with
DAIF masked. No DTB parsing or generic platform backend is introduced.

The verified handoff is EL1, MMU and caches off, DAIF masked, with x0 carrying
the firmware-selected DTB pointer and x1..x3 zero. The observed x0 address is
not a future ABI constant. Preserve the original pointer in the handoff record;
the vendor loader does not honor the normal third bootm argument for this
legacy image. Detailed commands and evidence belong to the platform README.

## Invariants

- Every instruction/data dependency is low-linked inside `.boot.*` storage.
- H618 constants never enter QEMU or generic kernel code.
- The checkpoint has no heap, runtime library, interrupt, timer, scheduler,
  userspace, or secondary-CPU dependency.
- Upload and execution use RAM only; no saved environment or media writes.
- `mkimage` owns wire-format serialization; timestamp and payload are checked.
- Existing firmware owns DRAM/security setup and handoff. Unverified firmware
  variants require fresh inspection rather than assuming this board's values.

## Consequences

This gives a physical, reproducible first checkpoint without claiming a full
hardware port. The temporary fixed UART setup and unbounded polling are
acceptable only in this isolated early bring-up path. Later DT-driven runtime
work must explicitly replace this boundary and validate incoming firmware data.
The firmware's inconsistent RAM-size reports are not imported into genrt policy.

## Alternatives considered

- Reuse QEMU's entry: rejects actual hardware/layout/firmware differences.
- Use `go` or `booti`: incompatible with the installed loader's execution mode
  or available commands.
- Replace firmware or regenerate SD images: unnecessary for the checkpoint.
- Add generic UART/DT/SMP support now: exceeds the first-output milestone.

## Validation

Physical UART uploads and explicit `bootm` entry succeeded twice with identical
image bytes, including an operator-confirmed clean power cycle. Both reached
the marker and EL1 diagnostics and stayed parked. The build performs boot-text
autonomy checks; host tests check wrapper fields, bounds, and payload identity.
Existing QEMU contracts remain the runtime regression gate, not a substitute
for the hardware evidence.

## Firmware compatibility addendum

The original board subsequently stopped completing BOOT0 DRAM initialization,
with no established cause. Historical UART success remains evidence of those
runs, not of ongoing board reliability.

A replacement board uses U-Boot 2024.10 and TF-A 2.11 from MMC. Standard
AArch64 U-Boot requires an ARM64 image tag and an explicit DTB; the vendor ARM
tag would select the wrong execution state. The default
`cargo xtask build-orangepi-zero2w` now emits an ARM64-tagged
`genrt-orangepi-zero2w.uImage`. Its payload also has the 64-byte ARM64 Image
header required by standard U-Boot's `bootm` load stage; tagging the old flat
payload alone was insufficient and failed before entry. A branch skips the
header without changing incoming registers. Zero text offset and placement
flag bit 3 retain the linked, 2 MiB-aligned load address. The declared image
size and serialized bytes include handoff storage and the private stack so
U-Boot reserves the entire footprint. UART and park behavior are unchanged.
There is no firmware selector or vendor wrapper. The standard U-Boot procedure
uses a validated copy of the control DTB for boot-time fixups, not the live
control tree. No new generic kernel or runtime hardware policy is introduced.
The first replacement-board attempt failed before genrt; successful mainline
handoff still requires physical verification.

## Related decisions

- [Retirement of the active checkpoint platform](ADR-0052-retire-hardware-uart-checkpoint.md).
- [ADR-0015: High-half MMU bring-up](ADR-0015-aarch64-high-half-mmu-bring-up.md).
