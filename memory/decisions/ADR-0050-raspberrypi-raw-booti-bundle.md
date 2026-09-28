# ADR-0050: Raspberry Pi raw booti bundle

## Status

Superseded by [ADR-0051](ADR-0051-raspberrypi-runtime-spin-table-and-power.md).
The runtime requires a ramdisk; the installed U-Boot now supports FIT.

Accepted. Partially supersedes [ADR-0049](ADR-0049-raspberrypi-uboot-uart-checkpoint.md)
for the default Raspberry Pi upload format and address only.

## Context

The installed Raspberry Pi U-Boot reports `Unknown image format!` for a FIT
whose size and CRC match the host artifact. It provides `booti`; the presence
of `bootm` alone is not evidence of FIT support. A single kernel/DTB UART
transfer must remain possible without replacing firmware.

## Decision

The existing Raspberry Pi build command additionally emits `-boot.bin`:
the verified ARM64 Image including BSS, zero padding to a 4 KiB boundary,
and the exact repository-built DTB. This is the default upload artifact.
Generate `BOOT.txt` with `loady`, the filename, exact `booti` arguments,
bundle size, and SHA-256. The DTB address is computed, never copied from a
previous build. Retain the verified FIT as an optional artifact.

Load at the linked base `0x02000000`, not the FIT staging address. The existing
zero text offset, placement flag, and 2 MiB-aligned base let `booti` retain
the kernel's linked location. Its normal firmware handoff receives the
appended DTB explicitly and may relocate/fix up the tree. No loader, board
entry, kernel runtime, UART policy, or persistent environment is changed.

## Invariants

- The ARM64 Image size covers kernel storage only, including serialized BSS.
- The DTB starts outside that extent and on a 4 KiB boundary.
- The whole bundle fits the existing 8 MiB kernel load window.
- Both address arithmetic and size bounds are checked before allocation.
- Operators still verify placement against actual RAM/LMB reservations.
- Build-time and transfer checks do not establish physical boot success.

## Consequences

No FIT support, second transfer, control-DTB copying, or SD write is required.
The raw bundle has no FIT hash enforcement in firmware; YMODEM transport checks
and the recorded host digest do not constitute secure boot. Addresses in
`BOOT.txt` must travel with their matching artifact. Orange Pi and release
QEMU packaging remain unchanged.

## Alternatives considered

- Rebuild U-Boot with FIT support: unnecessary firmware scope for a checkpoint.
- Send Image and DTB separately: violates the single-transfer requirement.
- Use a fixed large DTB offset: wastes UART transfer time on padding.
- Jump directly with `go`: bypasses the supported ARM64 firmware handoff.

## Validation

Host tests cover exact bytes, unchanged Image header, alignment at and across
boundaries, zero padding, deterministic output, size limits, and address
overflow. The hardware build validates the underlying Image and DTB before
bundling. Physical upload and repeated cold boot remain separate checks.

## Related decisions

- [ADR-0048](ADR-0048-orangepi-shared-boot-and-fit.md).
- [Raspberry Pi bootstrap](../../arch/aarch64/platform/raspberrypi-3b-plus/README.md).
- [U-Boot booti](https://docs.u-boot.org/en/latest/usage/cmd/booti.html).
- [U-Boot ARM64 placement](https://github.com/u-boot/u-boot/blob/v2024.10/arch/arm/lib/image.c).
