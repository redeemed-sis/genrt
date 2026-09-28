# ADR-0052: Retire hardware UART checkpoint

## Status

Accepted. Supersedes [ADR-0047](ADR-0047-orangepi-zero2w-uart-checkpoint.md)
and [ADR-0048](ADR-0048-orangepi-shared-boot-and-fit.md) for the active
Orange Pi build, boot, and UART-checkpoint boundary. Pi runtime ownership
remains with [ADR-0051](ADR-0051-raspberrypi-runtime-spin-table-and-power.md).

## Context

The Orange Pi Zero 2W target only printed early UART diagnostics and parked.
It did not enter the kernel runtime, scheduler, or userspace. Raspberry Pi 3B+
now uses the shared low entry to reach the runtime and has its own UART,
interrupt, CPU-release, and power backends. Keeping the Orange Pi feature and
build command would advertise a platform that cannot boot genrt.

The Pi runtime still requires U-Boot's ARM64 Image header, validated incoming
EL/translation state, and the firmware DTB pointer. Those are runtime handoff
requirements, not checkpoint instrumentation.

## Decision

Remove the Orange Pi checkpoint feature, its H618 polling UART and formatter,
the diagnostic handoff storage and park branch, board FIT/DTS inputs, and the
checkpoint-only xtask command. The hardware FIT builder now builds only the
Raspberry Pi 3B+ runtime image.

Keep the Pi firmware handoff in the shared boot entry, gated by the Pi feature.
Keep the normal Pi and QEMU runtime UART backends. Terminal parking remains a
containment path for invalid boot/secondary state or impossible return, not a
diagnostic UART checkpoint.

## Invariants and consequences

- QEMU's bare-metal entry and Pi's U-Boot entry remain separate compile-time
  paths through one boot assembly and linker contract.
- No Orange Pi runtime support is claimed. Reintroducing it requires verified
  H618 runtime backends and a new hardware decision.
- Production images contain no UART checkpoint formatter or handoff record.
- Historical hardware observations in ADR-0047 and ADR-0048 remain intact.

## Alternatives

- Retain an opt-in checkpoint build: leaves an unsupported, non-runtime target
  on the active command surface.
- Port the H618 runtime now: requires independently verified interrupt, timer,
  memory, CPU-start, and power behavior and is outside this cleanup.

## Validation

Check the Pi FIT builder and host tests, Pi post-link boot autonomy, QEMU
contracts, and searches for active checkpoint symbols and broken links.
Physical Pi runtime validation remains a separate operator check.

## Related decisions

- [ADR-0047: Original Orange Pi UART checkpoint](ADR-0047-orangepi-zero2w-uart-checkpoint.md).
- [ADR-0048: Orange Pi shared boot and FIT](ADR-0048-orangepi-shared-boot-and-fit.md).
- [ADR-0051: Raspberry Pi runtime](ADR-0051-raspberrypi-runtime-spin-table-and-power.md).
