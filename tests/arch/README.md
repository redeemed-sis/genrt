# BCM2837 backend checks

`cargo test -p xtask --locked` compiles the real Pi UART (including shared
PL011 register operations), interrupt and power backends with host MMIO mocks.
Tests check bounded UART RX/TX and firmware adoption,
CPU-local register offsets, simultaneous IRQ arbitration, mailbox clear ordering,
and watchdog password/reset/halt fields. CPU barriers are not modeled on hosts.

## Manual BCM2837 emulator smoke

This is a diagnostic smoke, not a protocol-based CI contract. It uses QEMU's
`raspi3b` model and its firmware spin loops. A tiny test-only handoff configures
PL011 and jumps to the **unmodified production Pi Image**. It bypasses U-Boot
FIT loading and cannot verify VideoCore firmware behavior or real cache timing.
Run from the repository root:

```bash
cargo xtask build-raspberrypi-3b-plus
mkdir -p target/hardware/raspberrypi-emulated
clang --target=aarch64-none-elf -c tests/arch/raspberrypi_handoff.S \
  -o target/hardware/raspberrypi-emulated/handoff.o
ld.lld --image-base=0x80000 -Ttext=0x80000 --oformat=binary \
  target/hardware/raspberrypi-emulated/handoff.o \
  -o target/hardware/raspberrypi-emulated/handoff.bin
cp target/raspberrypi-3b-plus/genrt-raspberrypi-3b-plus.dtb \
  target/hardware/raspberrypi-emulated/board.dtb
fdtput -t x target/hardware/raspberrypi-emulated/board.dtb \
  /chosen linux,initrd-start 0 04000000
end=$(printf '%x' "$((0x04000000 + $(stat -c %s target/raspberrypi-3b-plus/initramfs.cpio)))")
fdtput -t x target/hardware/raspberrypi-emulated/board.dtb \
  /chosen linux,initrd-end 0 "$end"
timeout 120s qemu-system-aarch64 -M raspi3b -display none \
  -serial stdio -monitor none \
  -kernel target/hardware/raspberrypi-emulated/handoff.bin \
  -device loader,file=target/raspberrypi-3b-plus/genrt-raspberrypi-3b-plus.bin,addr=0x02000000,force-raw=on \
  -device loader,file=target/hardware/raspberrypi-emulated/board.dtb,addr=0x07000000,force-raw=on \
  -device loader,file=target/raspberrypi-3b-plus/initramfs.cpio,addr=0x04000000,force-raw=on
```

Wait for the shell, then enter commands separately:

```text
taskset 2 echo pi-secondary
taskset 3 sleep 1
poweroff
```

Verify the echo and return from sleep; poweroff should terminate this QEMU model.
For a separate reboot run, use `reboot`, observe a fresh boot, then `poweroff`.
The timeout bounds failed tests and returns 124, which is not successful guest
poweroff. On real hardware reset returns to U-Boot, not directly to genrt.
