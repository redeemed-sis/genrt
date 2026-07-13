# Tagged releases

## Tags and gates

Release labels use v-prefixed SemVer, for example `v0.2.0` or
`v0.3.0-rc.1`. A real tag must point to `HEAD`, be reachable from `main`, and
be built from a clean worktree. A nonexistent label may still be used locally:

```bash
cargo xtask dist --tag v0.0.0-test.1 --output-dir /tmp/genrt-dist
```

`dist` runs the canonical checks and QEMU suites, then builds the production
release kernel and userspace ELF set once. The exact kernel is dynamically run
with userspace and shell contract initramfs images. Those images reuse the same
production ELF files later staged into the release image.

`user/c/programs.toml` is the sole declaration of release executable names,
sources, install paths, and contract roles. Build, release staging, contract
staging, identity comparison, and the tested/structural-only coverage report
are derived from that manifest. Source paths must resolve to regular files
strictly below the repository `user/c` tree; absolute paths, parent traversal,
and symlink escapes are rejected.

`tests/qemu/program-contracts.toml` binds every dynamic product one-to-one to
its protocol case, exact contract install path, argv, and expected exit code.
xtask generates the supervisor C table from this plan, so contract reporting is
not inferred merely from an ELF being present in the test image.

The production initramfs is not driven through a content-specific shell smoke.
Instead, xtask reopens the newc archive and compares every canonical path,
kind, mode, size, and SHA-256 with its generated manifest. Executable entries
must be ELF64 little-endian AArch64 `ET_EXEC`; duplicates, traversal paths,
non-production provenance, test-marker metadata, and the
`GENRT_TEST_ARTIFACT_V1` payload are rejected. Executable protocol magic is an
additional defense-in-depth check. Human documentation may mention the
protocol, and ordinary `test`/`fixtures` path names are not reserved. Repeated serialization of the
same staging tree must be byte-identical.

## Bundle

```text
genrt-aarch64-qemu-virt-vX.Y.Z.tar.gz
SHA256SUMS
```

The archive contains `genrt-aarch64.elf`, `qemu-virt.dtb`, `initramfs.cpio`,
`initramfs.manifest.json`, `manifest.json`, and `RUN.md`. The bundle manifest
records SemVer prerelease status, commit, tool versions, target/profile/features,
and file hashes. Archive ordering and metadata are deterministic.

## GitHub workflow

The read-only verification job invokes `cargo xtask dist`; only the dependent
publication job has contents write permission. Prerelease status comes from the
SemVer-derived bundle manifest. Runs for the same tag are serialized. A rerun
repairs a partial draft by uploading only missing assets after verifying every
existing asset byte-for-byte, then checks the complete inventory and
`SHA256SUMS` before publishing. Existing conflicting or unexpected assets are
never overwritten.
