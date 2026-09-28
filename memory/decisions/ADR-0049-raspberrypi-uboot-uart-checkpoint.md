# ADR-0049: Raspberry Pi 3B+ U-Boot UART checkpoint

## Status

Partially superseded by [ADR-0051](ADR-0051-raspberrypi-runtime-spin-table-and-power.md)
for the Pi early park and UART execution boundary.

Accepted. Extends ADR-0048 to a second explicit hardware target; does not
supersede Orange Pi's register programming or the QEMU runtime contract.

Partially superseded by [ADR-0050](ADR-0050-raspberrypi-raw-booti-bundle.md)
for the default Raspberry Pi upload format and address. UART adoption, shared
entry, and checkpoint boundaries remain unchanged.

## Context

Hardware development temporarily moves to Raspberry Pi 3B+ with an installed
U-Boot. Replacing addresses in the H618 driver is insufficient: BCM2837 has
different RAM/MMIO placement and can route either mini UART or PL011 onto the
debug header. Mini UART baud depends on the firmware-managed VPU clock.

The required scope remains full-kernel composition through shared boot code,
one kernel/DTB upload, UART diagnostics, and park, not a runtime board port.

## Decision

Retain Orange Pi support and add `build-raspberrypi-3b-plus` to the shared
hardware FIT builder. Each board explicitly selects its feature, source tree,
isolated Cargo target, upload address, and linked physical base. Raspberry Pi
uses upload `0x01000000`, entry `0x02000000`, and an 8 MiB kernel bound. Board
operators must confirm those windows against their actual U-Boot RAM/LMB map.

Both targets use the existing ARM64 header, shared `boot.s`, linker, validated
firmware handoff, and fixed boot-stack ownership. A nonzero Aff0 CPU parks
before shared memory is touched. All full-runtime symbols remain linked.
Only 64-bit U-Boot handoff at EL1/EL2 with MMU/data cache off is supported.

Factor formatting and handoff layout into low-linked `uart::checkpoint`.
H618 retains its existing hardware setup; BCM2837 adopts the UART already
configured by U-Boot. Read GPIO14/15 pinmux and transmitter state to select
ALT0 PL011 or ALT5 mini UART. Preserve clocks, baud divisors, and routing;
unsupported state parks silently. No firmware mailbox or guessed clock rate
is needed. Only polling TX is implemented, with no runtime RX/IRQ facade.

Compile a repository-owned minimal board DTS and package it in the FIT. The
Pi tree describes a conservative low-memory checkpoint window and both UART
register ranges, not complete Linux/runtime bindings or fixed stdout routing.
It does not replace the firmware's U-Boot control tree. Incoming x0 is recorded
but not dereferenced by the checkpoint.

## Invariants

- No hardware checkpoint reaches QEMU runtime MMIO, page tables, or scheduler.
- Exactly one hardware feature may be enabled; hardware and test features
  cannot be combined.
- All checkpoint code, constants, and diagnostic bytes remain low-linked.
- Raspberry Pi UART adoption does not mutate clocks, pinmux, or baud settings.
- The caller supplies a working 115200 8N1 header UART with stable clocks.
- Build commands do not access serial ports, firmware, or persistent storage.
- UART diagnostic success is not evidence of a working runtime kernel port.

## Consequences

The same builder and diagnostic code serve both boards without another entry
assembly file or duplicate packaging logic. Hardware-specific register policy
remains explicit and independently testable. The checkpoint depends on the
U-Boot UART handoff and deliberately does not bootstrap a disabled serial port.

The BCM2837 peripheral map is not a GICv2 platform. A future runtime port must
separately address interrupt, timer, memory, CPU startup, and console ownership.

## Alternatives

- Replace Orange Pi files: loses existing work and hides platform differences.
- Reuse QEMU PL011 initialization: assumes the wrong mapped base and clock and
  does not support the Pi's default mini UART.
- Force a new clock/pinmux using a firmware mailbox: expands hardware scope
  despite an already working loader console.
- Direct firmware boot: a different firmware/secondary-entry contract, not the
  requested installed-U-Boot path.

## Validation

Shared artifact checks verify ARM64 header bounds, boot autonomy, complete
runtime symbols, FIT/DTB metadata, extracted bytes, hashes, and deterministic
serialization. Host tests exercise the actual BCM2837 driver with mock MMIO,
including rejection and polling paths. QEMU CI protects the active runtime.
Physical upload and repeated cold boot remain separate, unverified evidence.

## Related decisions

- [ADR-0048](ADR-0048-orangepi-shared-boot-and-fit.md).
- [Raspberry Pi procedure](../../arch/aarch64/platform/raspberrypi-3b-plus/README.md).
