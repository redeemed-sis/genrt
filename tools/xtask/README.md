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

## Build artifacts

AArch64 artifacts live below
`target/aarch64-unknown-none-softfloat/<profile>/`. The workflow builds:

- `genrt-aarch64.elf` and QEMU `virt` DTB;
- freestanding userspace ELF files declared by `user/c/programs.toml`;
- deterministic production and contract initramfs archives plus manifests;
- QEMU result logs below `target/test-results/`;
- verified release bundles in the requested output directory.

Product source paths are canonicalized and must stay below `user/c`. Production
program identity, install path, contract role, and exact invocation coverage are
manifest-driven.

## QEMU configuration

`xtask` keeps DTB generation and runtime machine options synchronized, uses
explicit GICv2, disables networking/display/monitor, and attaches DTB and
initramfs as data without changing the kernel entry PC. QEMU command preview,
interactive run, debug, and test paths share the same artifact model.

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

## Changing xtask

Keep workflow semantics here rather than duplicating them in shell or YAML. Add
unit tests for parsers, manifests, path policy, protocol state, and negative
trust-boundary cases. Run formatting, xtask tests, clippy, and `check`; QEMU or
release changes require `ci`, and release composition changes require `dist`.
