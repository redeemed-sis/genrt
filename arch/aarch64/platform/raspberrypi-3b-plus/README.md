# Raspberry Pi 3B+ runtime bring-up

Experimental BCM2837B0 runtime through an installed **64-bit U-Boot with FIT
support**. The same kernel entry, scheduler, filesystem and userspace used on
QEMU are linked here, with Pi-specific IRQ, UART, CPU-release and power backends.

The runtime has reached the shell on QEMU's BCM2837 model, including
secondary CPU commands. The user also supplied physical-Pi rtbench results
on CPU1 and CPU3; those are distinct from emulator validation and do not
certify worst-case timing. Capture the serial log and repeat cold boot on
the real board. Choose UART transfer for RAM-only iterations or
copy the FIT onto the existing microSD boot partition for persistent loading.

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
| `genrt-raspberrypi-3b-plus.itb` | Kernel + DTB + initramfs; transfer over UART or copy to microSD |

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
| FIT staging in RAM | 0x01000000, below 0x02000000 |
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

## Boot over UART

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

The default FIT configuration supplies kernel, DTB and ramdisk in one upload.
This path leaves the microSD contents and saved U-Boot environment untouched.

## Boot from microSD

The card needs a FAT boot partition with Raspberry Pi firmware, a 64-bit
U-Boot, and `config.txt`. The steps below prepare those files without installing
Linux. If they already work on your card, skip to **Copy the FIT**.
The FIT is not a raw disk image and must not be written over the card with `dd`.

### Prepare the boot partition

For a blank card, create an MBR partition table and a first FAT32 partition
using a partitioning tool. This erases existing data: identify the card by size
and device name before formatting. An existing working FAT boot partition can
be reused; back up its contents before replacing firmware or configuration.

Identify and mount the partition on the host. These examples run from the genrt
repository root, in one shell; replace `/dev/sdX1` with the actual **partition**
(for example `/dev/mmcblk0p1`), not the whole disk:

```bash
lsblk -o NAME,SIZE,FSTYPE,LABEL,MOUNTPOINTS
pi_boot_mount=/mnt/pi-boot
pi_bootstrap_dir="$PWD/target/hardware/raspberrypi-3b-plus/bootstrap"
mkdir -p "$pi_bootstrap_dir"
sudo mkdir -p "$pi_boot_mount"
sudo mount -o "uid=$(id -u),gid=$(id -g)" /dev/sdX1 "$pi_boot_mount"
```

If the desktop already mounted the card, set `pi_boot_mount` to that writable
mount point instead of mounting it again.

### Install Raspberry Pi firmware

Clone the official firmware repository, checking out only its `boot` directory:

```bash
git clone --depth 1 --filter=blob:none --sparse \
  https://github.com/raspberrypi/firmware.git "$pi_bootstrap_dir/firmware"
git -C "$pi_bootstrap_dir/firmware" sparse-checkout set boot
git -C "$pi_bootstrap_dir/firmware" rev-parse HEAD
```

Record the printed commit for repeatable setup. Copy the Pi 3B+ boot files and
their license notices from that same checkout; `start.elf` and `fixup.dat` must
remain a matched pair:

```bash
cp "$pi_bootstrap_dir/firmware/boot/"{bootcode.bin,start.elf,fixup.dat} \
  "$pi_boot_mount/"
cp "$pi_bootstrap_dir/firmware/boot/bcm2710-rpi-3-b-plus.dtb" \
  "$pi_boot_mount/"
cp "$pi_bootstrap_dir/firmware/boot/"{LICENCE.broadcom,COPYING.linux} \
  "$pi_boot_mount/"
mkdir -p "$pi_boot_mount/overlays"
cp "$pi_bootstrap_dir/firmware/boot/overlays/disable-bt.dtbo" \
  "$pi_boot_mount/overlays/"
```

The overlay is used by the UART configuration below. Linux `kernel*.img` files
and modules from the firmware repository are not needed: firmware will start
U-Boot, which then loads genrt.

### Build and install U-Boot

Install an AArch64 GNU cross toolchain (`aarch64-linux-gnu-gcc` and binutils),
host GCC/make, Git, Bison, Flex, Python 3 with pyelftools, and OpenSSL development
headers. These are additional U-Boot build dependencies; see the
[upstream build guide](https://docs.u-boot.org/en/latest/build/gcc.html)
for distribution-specific packages.

Use the 64-bit `rpi_3_b_plus_defconfig`. This example pins U-Boot to `v2026.07`,
the version used in the recorded board session:

```bash
git clone --depth 1 --branch v2026.07 \
  https://source.denx.de/u-boot/u-boot.git "$pi_bootstrap_dir/u-boot"
make -C "$pi_bootstrap_dir/u-boot" CROSS_COMPILE=aarch64-linux-gnu- \
  rpi_3_b_plus_defconfig
"$pi_bootstrap_dir/u-boot/scripts/config" \
  --file "$pi_bootstrap_dir/u-boot/.config" \
  --enable FIT --enable HASH --enable BOOTMETH_SCRIPT \
  --enable CMD_SOURCE --enable LEGACY_IMAGE_FORMAT
make -C "$pi_bootstrap_dir/u-boot" CROSS_COMPILE=aarch64-linux-gnu- \
  olddefconfig
make -C "$pi_bootstrap_dir/u-boot" CROSS_COMPILE=aarch64-linux-gnu- \
  -j"$(nproc)"
cp "$pi_bootstrap_dir/u-boot/u-boot.bin" "$pi_boot_mount/u-boot.bin"
```

FIT support is required for `genrt.itb`; CRC32 is enabled by this U-Boot
configuration. Script support and the legacy image format allow the packaged
`boot.scr` used below. No custom firmware stub or PSCI implementation is needed.

### Create config.txt

Create this text file at the root of the FAT partition:

```bash
tee "$pi_boot_mount/config.txt" <<'EOF'
arm_64bit=1
kernel=u-boot.bin
device_tree=bcm2710-rpi-3-b-plus.dtb
enable_uart=1
dtoverlay=disable-bt
EOF
```

`arm_64bit` selects AArch64 and `kernel` makes firmware load U-Boot rather than
a Linux image. `device_tree` selects the firmware's Pi 3B+ tree, not genrt's
minimal runtime tree: the latter is already inside the FIT. `enable_uart`
enables the console; `disable-bt` disables Bluetooth and routes PL011 UART0 to
GPIO14/15 for the 115200-baud connection described above. genrt adopts that UART
configuration without changing its clock, divisors, or pinmux.

No Linux root partition or `cmdline.txt` is required for this boot path. Continue
by copying the FIT, then sync and unmount the card before inserting it into the
Pi. On first power-up, verify that U-Boot appears on UART and accepts commands
before attempting to start genrt.

### Copy the FIT

Build the image as above. Use the mounted FAT partition from the preparation
steps, or set `pi_boot_mount` to the mount point of your existing working card.
From the repository root:

```bash
pi_boot_mount=/mnt/pi-boot
cp target/raspberrypi-3b-plus/genrt-raspberrypi-3b-plus.itb \
  "$pi_boot_mount/genrt.itb"
sync
sudo umount "$pi_boot_mount"
```

Unmount the partition before removing the card. Keep the working firmware,
U-Boot, and `config.txt` files in place. The FIT already includes the runtime
DTB and initramfs; neither needs a separate copy.

### Load manually

Stop autoboot and identify the card and its FAT partition:

```text
mmc list
part list mmc 0
fatls mmc 0:1 /
```

The observed board exposes the boot partition as `mmc 0:1`. If the listing shows
a different device or partition, replace `0:1` in all commands below.

```text
load mmc 0:1 01000000 /genrt.itb
iminfo 01000000
bootm 01000000
```

Run `bootm` after a successful load and component integrity check. As with
UART, the FIT is staged at `0x01000000`; U-Boot places its kernel at the FIT's
`load` address, `0x02000000`, and supplies the DTB and initramfs.
genrt mounts that initramfs in RAM. U-Boot performs the microSD read; this does
not add an SD driver or writable filesystem to genrt.

### Configure automatic boot

After manual boot succeeds, create a host text file named `boot.cmd` with:

```text
if load mmc 0:1 01000000 /genrt.itb; then
    bootm 01000000
fi
```

Package it as a U-Boot script image and inspect the result:

```bash
mkimage -A arm64 -T script -C none -n genrt -d boot.cmd boot.scr
mkimage -l boot.scr
```

Remount the card after the manual boot check. Copy the generated `boot.scr` to
the root of the same FAT partition, using `pi_boot_mount` from above:

```bash
cp boot.scr "$pi_boot_mount/boot.scr"
sync
```

Back up any existing boot script before replacing it, then unmount the card.
Renaming the plain text file to `boot.scr` is insufficient: `source` requires
a script image and otherwise reports `Wrong image format for "source" command`.

Automatic discovery requires `CONFIG_BOOTMETH_SCRIPT`, `CONFIG_CMD_SOURCE`,
`CONFIG_LEGACY_IMAGE_FORMAT`, and a `bootcmd` using `bootflow scan`, as in the
observed U-Boot build. Its script boot method searches `/` and `/boot/` for
`boot.scr.uimg` first, then `boot.scr`. Check for an older `boot.scr.uimg` or
other boot configurations if another image is selected. This procedure needs
no `saveenv`; remove the generated boot script to undo automatic genrt loading.

## Verify runtime

Both loading paths should reach normal kernel initialization, four scheduler
CPUs, initramfs mount, and the `/init` shell. Logs are diagnostic milestones,
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
U-Boot. UART-only loading requires another transfer; a selected microSD
`boot.scr` loads genrt again. In a separate boot test `poweroff`: firmware halts
after a watchdog reset, but the board still has supply voltage. Power-cycle to
restart. Both commands affect the whole board, even on a secondary CPU.

## Periodic timing

To measure periodic execution, rebuild with
`cargo xtask build-raspberrypi-3b-plus`, deploy the resulting FIT, and run:

```text
taskset 3 rtbench -p 8000 -n 1000 -o 30 -v
taskset 3 rtbench -p 15000 -n 1000 -o 30 -v
```

The `-v` flag enables per-STEP output; omit it for summary-only runs.
Retain the full STEP output and SUMMARY with the FIT identity and firmware
setup. See the [latest hardware results](../../../../docs/testing.md#raspberry-pi-hardware-result-user-reported).
QEMU checks functionality; it does not establish physical-board timing bounds.

For diagnostic phase collection, build with
`cargo xtask build-raspberrypi-3b-plus --wake-profile` and deploy the FIT from
`target/raspberrypi-3b-plus/wake-profile/`, not the ordinary image directory.
The [profiling guide](../../../../docs/wake-profiling.md) describes full and
focused console reports and matched overhead runs. The existing timing
results above are not phase profiles.

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

- [Raspberry Pi firmware boot files](https://github.com/raspberrypi/firmware/tree/master/boot).
- [Raspberry Pi firmware configuration](https://www.raspberrypi.com/documentation/computers/config_txt.html).
- [Firmware UART overlays](https://www.raspberrypi.com/documentation/computers/configuration.html#configure-device-tree-overlays-for-uarts).
- [U-Boot Raspberry Pi configurations](https://docs.u-boot.org/en/latest/board/broadcom/raspberrypi.html).
- [U-Boot GCC build instructions](https://docs.u-boot.org/en/latest/build/gcc.html).
- [U-Boot filesystem loading](https://docs.u-boot.org/en/latest/usage/cmd/load.html).
- [U-Boot script image format](https://docs.u-boot.org/en/latest/usage/cmd/source.html).
- [U-Boot automatic script discovery](https://docs.u-boot.org/en/latest/develop/bootstd/script.html).
- [BCM2835 registers](https://datasheets.raspberrypi.com/bcm2835/bcm2835-peripherals.pdf).
- [Firmware armstub8](https://github.com/raspberrypi/tools/blob/master/armstubs/armstub8.S).
- [BCM2837 CPU release metadata](https://github.com/torvalds/linux/blob/master/arch/arm/boot/dts/broadcom/bcm2837.dtsi).
- [ARM-local IRQ definitions](https://github.com/torvalds/linux/blob/master/include/linux/irqchip/irq-bcm2836.h).
- [Watchdog and firmware halt convention](https://github.com/torvalds/linux/blob/master/drivers/watchdog/bcm2835_wdt.c).
- [ARM64 handoff](https://www.kernel.org/doc/html/latest/arch/arm64/booting.html).
- [ADR-0051](../../../../memory/decisions/ADR-0051-raspberrypi-runtime-spin-table-and-power.md).
