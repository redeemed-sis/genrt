# xtask workflow

`cargo xtask` is the canonical implementation of repository build, QEMU, test,
initramfs, and release semantics. `just` provides short aliases; GitHub Actions
install dependencies and delegate to the same commands.

## Core commands

```bash
cargo xtask doctor
cargo xtask check
cargo xtask test-aarch64 --list
cargo xtask test-aarch64
cargo xtask ci
cargo xtask dist --tag v0.0.0-local.1 --output-dir /tmp/genrt-dist
```

Use `cargo xtask --help` for the complete current command surface.

Diagnostic `--wake-profile` builds use separate Cargo and linked artifact
directories. QEMU output goes below `target/wake-profile/`; Pi output goes below
`target/raspberrypi-3b-plus/wake-profile/`. Ordinary `ci` and `dist` never enable
profiling; release validation rejects the diagnostic kernel marker. See
[sleep/wakeup profiling](../../docs/wake-profiling.md) for collection and reports.

`cargo xtask build-raspberrypi-3b-plus` uses the shared hardware builder with
BCM2837 UART adoption and separate `target/raspberrypi-3b-plus` artifacts.
With the rebuilt FIT-capable U-Boot, upload `genrt-raspberrypi-3b-plus.itb`
at `0x01000000`, inspect it with `iminfo 01000000`, then `bootm 01000000`.
The FIT selects its embedded DTB, loads the kernel at `0x02000000`, and supplies
the production initramfs at `0x04000000` before firmware relocation.
Pi FIT components use CRC32 for compatibility with the installed loader;
the builder recomputes and checks each component. Confirm the installed 64-bit
U-Boot memory map and spin-table CPU metadata before use.
See the [Pi bootstrap procedure](../../arch/aarch64/platform/raspberrypi-3b-plus/README.md).
Pi enters an experimental SMP runtime with
firmware-inherited UART, CPU-local interrupts, spin-table release and watchdog
power control. The builder's tests compile real BCM2837 UART/controller/power
backends against mock MMIO, without requiring a board or AArch64 execution.

## Build artifacts

AArch64 artifacts live below
`target/aarch64-unknown-none-softfloat/<profile>/`. The workflow builds:

- `genrt-aarch64.elf` and QEMU `virt` DTB;
- freestanding userspace ELF files declared by `user/c/programs.toml`;
- a deterministic static userspace support archive built with `llvm-ar` and
  linked by the common production/example/test ELF build path;
- deterministic production and contract initramfs archives plus manifests;
- QEMU result logs below `target/test-results/`;
- verified release bundles in the requested output directory.

Product source paths are canonicalized and must stay below `user/c`. Production
program identity, install path, contract role, and exact invocation coverage are
manifest-driven.

The support archive is built from production userspace sources, without test
defines, marker includes, or generated invocation headers. Test programs receive
their own marker at compilation, outside the archive. Release identity checks
cover the resulting statically linked program bytes.

The ELF builder applies `-O0` to debug userspace and `-O2` to release userspace,
including CRT0, common library members, and test helpers. It retains
`-ffreestanding`, `-fno-builtin`, and `-mgeneral-regs-only` in both profiles.
The Raspberry Pi FIT uses the release userspace path. To exercise optimized
userspace explicitly, run `cargo xtask test-aarch64 --profile release`.

The host support test links portable library modules with `--wrap=write` to
inject output behavior. Guest builds instead link the common public `write()`
wrapper using architecture syscall transport and the sleep wrappers; test linker
options never enter production ELF builds.
The same host check builds the shell-private input/parser tests without write
interposition. `tests/user/shell.c` additionally executes the production command
path with child syscalls substituted by `tests/user/shell_host_include` only in
that host build, checking redirection flags, parent isolation and error cleanup.
Production shell history/completion is exercised separately by
`shell-contract`, whose `/bin` fixtures are explicitly marked test-only.

`check` also builds `tests/user/rtbench.c`, including the actual production
utility under a renamed entry point. Only this host target receives
`tests/user/rtbench_host_include`, which supplies controlled public clock
observations and syscall-backed sleeps. Shared formatting and parsing
remain real library implementations, with output injected via `--wrap=write`.
No test adapter path or production test conditional enters guest builds.

`tests/user/time.c` substitutes only the architecture counter adapter to test
public clock conversion, rounding, normalization, and error limits. Guest
builds instead disassemble the complete clock object, require physical counter
and frequency reads, reject SVC, and reject undefined helper dependencies.
This verifies the supported clock path at both debug and release optimization.

## QEMU configuration

`xtask` keeps DTB generation and runtime machine options synchronized, uses
explicit GICv2, disables networking/display/monitor, and attaches DTB and
initramfs as data without changing the kernel entry PC. QEMU command preview,
interactive run, debug, and test paths share the same artifact model.

`build-aarch64`, `run-aarch64`, `debug-aarch64` and `qemu-cmd --arch aarch64`
accept `--disk <existing.raw>`. Validation requires a sector-aligned regular
file. Modern virtio-mmio attachment arguments are shared with DTB generation;
disk-bearing DTBs have distinct names. Attachment never creates or formats a disk.
Without the option, canonical diskless arguments and release launcher behavior
remain unchanged. `storage-contract` alone creates an explicitly owned test
fixture and verifies two-boot persistence through mtools/fsck.
See [storage](../../docs/storage.md).

Create a separate empty data image explicitly:

```bash
cargo xtask disk-create --output target/data.raw --size-mib 128 --label GENRT
cargo xtask run-aarch64 --cpus 4 --disk target/data.raw
```

The size is the entire disk in MiB (minimum 65, default 128). By default, `sfdisk`
creates one FAT32 partition; `mkfs.fat` chooses cluster size. The parent must
exist and support hard links; existing output
entries, including symlinks, are never replaced. Formatting is bounded and
staged privately before publication. No kernel, initramfs or fstab is installed;
the normal absent-fstab fallback mounts the partition at `/mnt/disk`.
See [disk preparation](../../docs/storage.md#create-a-data-disk) for dependencies
and host-side file copying. `disk-create` does not change test fixture or release
composition.

For multiple partitions and initial files, pass
`--layout examples/storage/disk.toml` to use the supplied example. Its schema-1
`[[partitions]]` entries specify labels, optional source directories and sizes in
MiB; only the last size may be omitted to use remaining space. Up to four FAT32
partitions are supported. Source paths are relative to the TOML file. Sorted
contents, including dotfiles and empty directories, are copied with fixed FAT
timestamps before publication. Supply `fstab` in the first partition's source;
it is not generated implicitly. `--label` and `--layout` are mutually exclusive.
See the [multi-partition example](../../docs/storage.md#multiple-partitions-and-initial-files).

Child ownership includes bounded deadlines, concurrent UART/stderr draining,
termination, wait, and complete-log validation. See
[`tests/qemu/README.md`](../../tests/qemu/README.md).

## Determinism and trust boundaries

- Cargo uses the tracked lockfile and pinned toolchain.
- Archive entries are sorted and carry controlled mode, uid/gid, and timestamps.
- Serialization is repeated and compared where release identity depends on it.
- Production and test-only initramfs construction use separate policy entry
  points.
- Production verification rejects marker payloads/metadata, protocol code, test
  provenance, noncanonical paths, invalid executable ELF, and hash mismatch.
- Release packaging reuses already tested production artifacts and records
  tool versions, commit, features, and SHA-256 identity.
- `dist` also emits a standalone versioned Raspberry Pi 3B+ `.itb` and JSON
  sidecar. Its platform kernel is built and structurally verified separately;
  its production initramfs is copied byte-for-byte from the QEMU release without
  rebuilding userspace. The sidecar records this verification scope, features,
  component hashes, and hardware build tools. `SHA256SUMS` covers the QEMU
  archive, Pi FIT, and sidecar; unexpected output-directory entries are rejected.
- Release bundles include an executable `run.sh` and DTBs for all supported
  CPU counts. The launcher and standalone `RUN.md` commands are generated from
  `qemu::Config`, with one sorted manifest/archive inventory and repeated
  tar/gzip serialization. Launcher tests use a QEMU stub to compare exact argv,
  reject malformed input, and check runtime dependency and artifact failures.

## Changing xtask

Keep workflow semantics here rather than duplicating them in shell or YAML. Add
unit tests for parsers, manifests, path policy, protocol state, and negative
trust-boundary cases. Run formatting, xtask tests, clippy, and `check`; QEMU or
release changes require `ci`, and release composition changes require `dist`.
