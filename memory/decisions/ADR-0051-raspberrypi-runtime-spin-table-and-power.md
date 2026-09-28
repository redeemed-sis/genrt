# ADR-0051: Raspberry Pi runtime, spin-table and power

## Status

Accepted for experimental bring-up; physical runtime validation pending.
Partially supersedes [ADR-0049](ADR-0049-raspberrypi-uboot-uart-checkpoint.md)
for Pi early park/UART execution, and supersedes
[ADR-0050](ADR-0050-raspberrypi-raw-booti-bundle.md) for Pi artifact packaging.
Orange Pi retained its checkpoint at this decision's acceptance; the
checkpoint was later removed by
[ADR-0052](ADR-0052-retire-hardware-uart-checkpoint.md). QEMU retains its
existing contract target.

## Context

The operator's Pi 3B+ reached the shared UART checkpoint from 64-bit U-Boot at
EL2. Firmware reports spin-table CPU release words d8/e0/e8/f0, not PSCI.
The board uses BCM2837 ARM-local interrupts plus the GPU cascade, not GICv2.
Its reported RAM starts at zero and ends at 0x3b400000; upper U-Boot reservations
start at 0x39f39000. U-Boot now supports FIT and CRC32 but not SHA-256.

## Decision

The Pi feature enters the full common kernel through boot.s. A stackless
architecture-owned EL2-to-EL1 transition enables physical counter/timer access
before shared MMU setup. Secondary spin entry derives its bootstrap slot from
the checked Aff0 identity rather than assuming a PSCI context argument.

Validate ordered CPU0..3 spin-table metadata against the supported firmware
contract. Reserve low firmware storage. Publish the existing TTBR1 handoff,
clean tables and release word to PoC, then SEV. Generic startup retains exact
scheduler-online acknowledgement and its deadline; publication alone does not
establish successful CPU activation. Normal-memory atomics retain their existing
ownership model; the firmware armstub must have enabled Cortex-A53 coherency.

An architecture-local interrupt facade dispatches GICv2 on QEMU and BCM2837 on
Pi. CPU0 owns GPU routing and UART. Every CPU owns its timer and mailbox0 IRQs;
mailbox SET performs targeted scheduler notification. Simultaneous timer and
mailbox pending states use separate return checkpoints, leaving the unserved
source asserted. Do not consume remote ingress outside its scheduler API.

Pi runtime UART adopts the configured PL011 or mini UART through the existing
console ABI, with bounded TX polling/RX draining and no clock/pinmux changes.
PSCI is excluded from the Pi build. Reset uses the password-protected PM watchdog;
poweroff also sets firmware partition code 63 for halt. A terminal atomic claim
serializes requests; an unsuccessful watchdog attempt restores local IRQ state
and returns the existing architecture error ABI after a bounded counter wait.
Halt is not physical power removal. Other targets retain DTB-selected HVC/SMC.

Bundle kernel, local DTB and production initramfs in one CRC32 FIT. Validate all
three extracted components and deterministic serialization. Use the loader's
actual /chosen initrd range and reserve it before allocation. The low first MiB
and all RAM above 128 MiB remain excluded from allocation. Support PA-zero RAM
and reuse L2 tables when RAM and MMIO share an L1 slot. Before executable Pi page
publication, clean data to PoU and invalidate inner-shareable instruction caches.

## Consequences and invariants

- No generic kernel register encodings, new syscall ABI, or platform scheduler.
- No allocation in IRQ, IPI or power paths; storage remains preallocated.
- Spin-table cache traversal is boot-only, bounded by resident page tables.
- Hardware assumptions are explicit and board-scoped, not QEMU fallbacks.
- The old Image/DTB-only booti bundle is removed; it omitted the required ramdisk.
- Firmware reservations and clocks are preserved; no persistent writes occur.
- CRC32 is transport integrity only, not authentication or secure boot.
- SD, USB, networking, GPU drivers and broad board autodetection remain absent.

## Alternatives

- Inventing PSCI for Pi would trap without a firmware provider.
- A second entry assembly would duplicate stack/MMU and context contracts.
- Keeping the early park cannot exercise scheduler, initramfs or EL0.
- Mapping only the initially advertised 128 MiB would lose relocated payloads
  when U-Boot puts them higher in actual RAM; reserve high RAM instead.

## Validation

Mock-MMIO tests exercise actual UART, controller and PM writes. Both hardware
builds retain boot-autonomy and artifact checks. QEMU virt CI remains mandatory.
BCM2837 emulator smoke uses the same production Pi kernel with a synthetic
firmware handoff; it cannot certify electrical RX, cache behavior or VideoCore
firmware halt. Physical runtime/cold-boot confirmation remains separately required.

## References

- [Pi procedure](../../arch/aarch64/platform/raspberrypi-3b-plus/README.md).
- [Firmware armstub8](https://github.com/raspberrypi/tools/blob/master/armstubs/armstub8.S).
- [BCM2837 CPU metadata](https://github.com/torvalds/linux/blob/master/arch/arm/boot/dts/broadcom/bcm2837.dtsi).
- [BCM2836 local controller](https://github.com/torvalds/linux/blob/master/include/linux/irqchip/irq-bcm2836.h).
- [PM watchdog/halt protocol](https://github.com/torvalds/linux/blob/master/drivers/watchdog/bcm2835_wdt.c).
- [ARM64 boot protocol](https://www.kernel.org/doc/html/latest/arch/arm64/booting.html).
