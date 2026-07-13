# Debugging AArch64 QEMU

## Build and inspect commands

Use `xtask` as the source of QEMU and GDB command semantics:

```bash
just doctor
just qemu-cmd-aarch64
just gdb-cmd-aarch64
```

`qemu-cmd-aarch64` prints the paused QEMU command with its GDB stub. The normal
interactive recipes are:

```bash
just debug-aarch64
just gdb-aarch64
```

Run QEMU in one terminal and GDB in another. The kernel ELF is under
`target/aarch64-unknown-none-softfloat/<profile>/genrt-aarch64.elf`.

## QEMU contract failures

Re-run one case before the whole suite:

```bash
cargo xtask test-aarch64 --case kernel-contract
cargo xtask test-aarch64 --case shell-contract
```

Each case writes `serial.log`, `qemu-stderr.log`, and `result.json` below
`target/test-results/<case>/`. Inspect the complete serial log: human lines help
diagnosis, while `GTRT/1` records alone determine machine status.

Distinguish setup/link failures, protocol failures, guest assertions, and suite
timeouts. The runner terminates and reaps QEMU on every bounded failure path.

## Early boot failures

Pre-MMU failures may occur before normal logging. Verify the linked image with:

```bash
cargo xtask build-aarch64 --log-level debug
readelf -SW target/aarch64-unknown-none-softfloat/debug/genrt-aarch64.elf
readelf -rW target/aarch64-unknown-none-softfloat/debug/genrt-aarch64.elf
llvm-objdump -dr --section=.boot.text \
  target/aarch64-unknown-none-softfloat/debug/genrt-aarch64.elf
```

The build command already enforces `.boot.text` autonomy. Do not bypass that
check when changing boot code, the linker script, or low platform parsing.
