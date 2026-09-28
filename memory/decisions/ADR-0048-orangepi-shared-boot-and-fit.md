# ADR-0048: Orange Pi shared kernel entry and FIT packaging

## Status

Superseded for active Orange Pi support by
[ADR-0052](ADR-0052-retire-hardware-uart-checkpoint.md). This decision
partially superseded [ADR-0047](ADR-0047-orangepi-zero2w-uart-checkpoint.md)
for the separate assembly entry/linker, standalone-only linkage, and upload
packaging.

## Context

The original checkpoint verified a tiny assembly-only image on vendor firmware.
The replacement board uses standard 64-bit U-Boot and requires an ARM64 Image
header plus a supplied DTB. Maintaining another entry point duplicates bootstrap
ownership, while manually copying U-Boot's control tree makes each UART run
depend on firmware-internal addresses.

The requested next step is shared bootstrap and complete kernel composition,
not an unverified activation of QEMU platform drivers on H618.

## Decision

Use the existing AArch64 staticlib, shared `src/boot.s`, and a common
`link/kernel.ld`. QEMU retains its default base and normal initialization.
The `platform-orangepi-zero2w` feature adds the ARM64 Image header, incoming
firmware-state validation/capture, and a mandatory stop after the common boot
CPU stack setup. Only the low polling UART routines remain platform-specific.

Link the entire release staticlib with `--whole-archive` and without section
GC at physical `0x42000000`. High runtime VMAs and low LMAs retain the existing
kernel layout. The full memory extent, including trailing BSS, is serialized
so U-Boot reserves all linker-owned memory as loaded bytes. Runtime symbol
checks guard against accidentally returning to an assembly-only image.

Keep source `board.dts` and FIT description `image.its` in the platform
directory. Compile the minimal checkpoint DTB and package it alongside the
kernel in one deterministic `.itb`. The default FIT configuration references
both images and SHA-256 hashes. One UART `loady` followed by `bootm` selects
the complete boot input without a control-DTB copy or guessed pointer.

Build in a separate Cargo target directory. Validate both embedded components
against their source bytes and hashes after serialization, bound the upload
below the kernel load window, and compare repeated FIT bytes.

## Invariants

- There is one `_start`, one boot-stack layout, and one terminal park owner.
- Every reachable hardware-checkpoint instruction and datum remains low-linked.
- No Orange Pi path reaches QEMU page-table construction or runtime drivers.
- Kernel, scheduler, exception, syscall, and process code are linked but not
  exercised merely by printing the checkpoint marker.
- The board tree describes only verified checkpoint facts; it is not a full
  runtime device/clock/interrupt description.
- FIT transport uses RAM only and does not modify persistent board state.
- QEMU and hardware Cargo outputs are isolated; test features cannot be combined
  with the hardware checkpoint feature.
- Boot execution remains allocation-free with fixed stack and handoff storage.

## Consequences

UART transfers are larger because they carry the full kernel instead of a few
hundred bytes of assembly. The optimization profile and bounded zero padding
keep composition explicit. Future hardware work can advance the common boot
boundary without maintaining a second entry implementation.

The early park is not an optional runtime switch: removing it requires H618
MMU/platform, UART, interrupt, timer, and firmware policy work. Runtime code
presence must not be reported as a working kernel port.

## Alternatives considered

- Keep an independent entry: duplicates the bootstrap ownership being unified.
- Execute QEMU high Rust immediately: uses incompatible platform assumptions.
- Upload a separate DTB or copy the control tree: retains manual boot inputs.
- Embed a DTB only inside kernel rodata: does not expose it to U-Boot's boot
  preparation. FIT defines that relationship before kernel entry.
- Vendor a full upstream DTS tree: unnecessary for the current parked boundary;
  defer complete runtime bindings to the actual platform port.

## Validation

The hardware builder verifies boot autonomy, runtime symbol retention, ARM64
header fields and bounds, board properties, default FIT references, extracted
component bytes, SHA-256, and deterministic serialization. Host tests cover
header corruption and memory/upload bounds. QEMU CI and release checks protect
the shared entry/linker path. Physical FIT handoff and a repeated cold boot
remain required hardware evidence; old standalone runs do not establish them.

## Related decisions

- [ADR-0015: High-half MMU bring-up](ADR-0015-aarch64-high-half-mmu-bring-up.md).
- [ADR-0047: Original UART checkpoint](ADR-0047-orangepi-zero2w-uart-checkpoint.md).
- [Retirement of the active checkpoint platform](ADR-0052-retire-hardware-uart-checkpoint.md).
