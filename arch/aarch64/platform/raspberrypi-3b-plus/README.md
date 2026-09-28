# Raspberry Pi 3B+ runtime bring-up

Experimental BCM2837B0 runtime through an installed **64-bit U-Boot with FIT
support**. The same kernel entry, scheduler, filesystem and userspace used on
QEMU are linked here, with Pi-specific IRQ, UART, CPU-release and power backends.

The runtime has reached the shell on QEMU's BCM2837 model, including
secondary CPU commands.
This is **not physical runtime validation**. Capture the serial log and repeat
cold boot on the real board. Nothing below writes SD or saves U-Boot environment.

## Build

```bash
cargo xtask build-raspberrypi-3b-plus
```

Requires the Rust/AArch64 toolchain, Clang, LLD, LLVM tools (including llvm-ar),
dtc, fdtget, mkimage and dumpimage. UART transfer uses picocom and lrzsz.
Outputs under `target/raspberrypi-3b-plus/`:

| File | Purpose |
| --- | --- |
| `genrt-raspberrypi-3b-plus.elf` | Complete release kernel with symbols |
| `genrt-raspberrypi-3b-plus.bin` | ARM64 Image, including serialized BSS |
| `genrt-raspberrypi-3b-plus.dtb` | Repository board tree |
| `initramfs.cpio` | Production /init shell and /bin programs |
| `initramfs.manifest.json` | Verified archive inventory |
| `genrt-raspberrypi-3b-plus.itb` | **Upload this:** kernel + DTB + initramfs |

The builder checks boot autonomy, retained symbols, production policy, ARM64
metadata, address bounds, exact FIT component bytes/CRC32 and repeat serialization.

## Firmware and memory prerequisites

U-Boot must accept GPIO14/15 UART commands at 115200 8N1, and support FIT/CRC32
(`CONFIG_FIT`, `CONFIG_HASH`). SHA-256 and signatures are not required.
CRC32 detects corruption, not malicious modification.

Use a **3.3 V logic-level** adapter: Pi pin 6 GND to GND, pin 8 TX to adapter RX,
pin 10 RX to adapter TX. Do not connect adapter power outputs. Wire with power
off and power the board normally. Retain the working UART clocks and pinmux;
mini UART requires a stable VPU clock.

Before the first runtime attempt record:

```text
version
bdinfo
fdt addr ${fdtcontroladdr}
fdt print /cpus
```

The supplied tree follows the observed spin-table contract: CPU0 release word
`0xd8`, CPU1 `0xe0`, CPU2 `0xe8`, CPU3 `0xf0`. CPU identities must be
ordered 0..3. Different enable methods or release addresses are rejected.
The first MiB stays reserved for firmware/stubs and release words.

| Use | Physical placement |
| --- | --- |
| FIT upload | 0x01000000, below 0x02000000 |
| Kernel, BSS and stacks | 0x02000000, below 0x02800000 |
| FIT initramfs load | 0x04000000, at most 4 MiB |
| Allocator ceiling | 0x08000000 (128 MiB) |

The reported board has RAM [0, 0x3b400000), with upper U-Boot reservations
starting at 0x39f39000. These load windows do not overlap them. Recheck with
different firmware. A reserved-memory interval excludes RAM above 128 MiB even
when U-Boot expands /memory; it protects firmware and framebuffer state.
U-Boot may relocate DTB and initramfs: genrt reserves and uses the actual
/chosen/linux,initrd-* extent, not the initial FIT load address.
Do not replace U-Boot's control DTB with this minimal runtime tree.

## Upload and boot

Open exactly one UART session:

```bash
mkdir -p target/hardware/raspberrypi-3b-plus
picocom -b 115200 --flow n \
  --send-cmd 'lrzsz-sz --ymodem -b -vv' \
  --logfile target/hardware/raspberrypi-3b-plus/serial.log /dev/ttyUSB0
```

Some distributions name the sender `sz`.

1. Stop autoboot and enter `loady 01000000`.
2. Press Ctrl-A, then Ctrl-S, and supply
   `target/raspberrypi-3b-plus/genrt-raspberrypi-3b-plus.itb`.
3. Wait for the prompt. Run `iminfo 01000000`: **all three** components must
   pass CRC32 verification.
4. Run `bootm 01000000`.

The default FIT configuration supplies kernel, DTB and ramdisk. No separate
transfer, fdt move, saveenv or SD write is needed. Expect normal kernel
initialization, four scheduler
CPUs, initramfs mount and the /init shell. Logs are diagnostic milestones,
not a machine test protocol.

Try individually, waiting for completion:

```text
pwd
ls /bin
cat /etc/banner
taskset 1 echo cpu1
taskset 2 sleep 1
taskset 3 echo cpu3
```

Only after recording the session, test `reboot`. It returns to firmware and
U-Boot; it does **not** reload the RAM-only genrt image automatically. In a
separate boot test `poweroff`: firmware halts after a watchdog reset, but the
board still has supply voltage. Power-cycle to restart. Both commands affect
the whole board, even when invoked on a secondary CPU.

## Architecture boundary

- Shared boot.s preserves the primary DTB, drops firmware EL2 to EL1 and enables
  common mappings. There is no separate Pi entry assembly.
- Spin-table CPUs enter the same low secondary path, derive their validated
  bootstrap slot from MPIDR and use private fixed stacks. CPU0 cleans the
  release word, TTBR1 handoff and tables before SEV. Generic boot waits for
  each exact scheduler-online acknowledgement with its existing timeout.
- The early DTB parser maps the peripheral window from `soc/ranges`;
  `platform/raspberrypi.rs` owns the ARM-local controller window.
  CPU0 owns GPU IRQ cascade/UART; each CPU owns its physical timer and mailbox0.
  Mailboxes provide targeted scheduler IPIs. Simultaneous timer and mailbox
  sources use separate IRQ-return checkpoints.
- `platform/uart/bcm2837_dtb.rs` resolves GPIO, PL011, AUX and mini UART
  `reg` addresses through the bus translation. `bcm2837.rs` adopts ALT0 PL011
  or ALT5 mini UART, preserving clocks, divisors and pins. Shared PL011 register
  operations live in `pl011.rs`; TX polling and RX draining stay bounded.
- platform/pi_power.rs owns password-protected watchdog registers and firmware
  halt flags. Pi never executes PSCI. Other platforms retain DTB-selected
  HVC/SMC. The syscall ABI does not change.
- PA-zero RAM is valid. MMIO mapping must not replace a RAM L1 descriptor.
  Executable pages receive cache maintenance before runnable publication,
  including fork placement onto another CPU.

Host tests in tests/arch exercise actual UART/controller/power code with mock
MMIO; `cargo test -p xtask --locked` runs them. They cannot establish electrical
behavior, firmware halt, cache coherency or hardware timing. QEMU virt remains
the canonical CI target; emulator smoke is not a substitute for physical boot.

## Sources

- [BCM2835 registers](https://datasheets.raspberrypi.com/bcm2835/bcm2835-peripherals.pdf).
- [Firmware armstub8](https://github.com/raspberrypi/tools/blob/master/armstubs/armstub8.S).
- [BCM2837 CPU release metadata](https://github.com/torvalds/linux/blob/master/arch/arm/boot/dts/broadcom/bcm2837.dtsi).
- [ARM-local IRQ definitions](https://github.com/torvalds/linux/blob/master/include/linux/irqchip/irq-bcm2836.h).
- [Watchdog and firmware halt convention](https://github.com/torvalds/linux/blob/master/drivers/watchdog/bcm2835_wdt.c).
- [ARM64 handoff](https://www.kernel.org/doc/html/latest/arch/arm64/booting.html).
- [ADR-0051](../../../../memory/decisions/ADR-0051-raspberrypi-runtime-spin-table-and-power.md).
