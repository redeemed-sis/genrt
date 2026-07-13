# Freestanding userspace

genrt userspace consists of statically linked freestanding AArch64 C programs.
There is no libc, dynamic linker, TLS runtime, ELF interpreter, or hosted process
startup environment.

## Product programs

`user/c/programs.toml` is the sole production program registry. Each entry
declares source, initramfs install path, contract role, and dynamic case. `xtask`
uses it for compilation, contract staging, release staging, identity checks, and
coverage reporting.

Current products are the shell and `/bin/echo`, `/bin/cat`, `/bin/ls`, and
`/bin/pwd`. Other example C files may be buildable without entering production
composition.

To add a production binary:

1. add freestanding C source under `user/c/`;
2. register it in `user/c/programs.toml`;
3. add an exact invocation to `tests/qemu/program-contracts.toml`;
4. update the owning supervisor contract and run the targeted QEMU case;
5. run `cargo xtask ci` and release composition checks when applicable.

## Build and startup

The common linker script creates ELF64 little-endian AArch64 `ET_EXEC` images
with static `PT_LOAD` segments. `user/c/aarch64/crt0.S` reads the kernel-built
initial stack, calls `main(argc, argv)`, and exits through the syscall ABI.

The initial stack and syscall conventions are documented in
[`user/c/aarch64/README.md`](c/aarch64/README.md). AArch64 wrappers and syscall
numbers stay under `user/c/aarch64/include/`.

## Initramfs

`user/initramfs/` contains product data files. `xtask` stages registered ELF
programs and this data into a deterministic readonly `newc` archive. `/init` is
the shell by default. The kernel parses the archive and loads ELF segments; QEMU
does not set the userspace PC.

## ABI boundaries

- Syscall number in `x8`, arguments in `x0..x5`, return in `x0`.
- Errors are negative errno values.
- Pathnames, argv/envp, I/O, and directory buffers are bounded.
- `fork` eagerly copies process memory; `execve` replaces it from ramfs.
- Shell line editing and command policy are userspace responsibilities.

Do not add libc assumptions or raw flat-binary loading paths. Syscall changes
require an ADR, synchronized kernel/header updates, and exact contract coverage.
