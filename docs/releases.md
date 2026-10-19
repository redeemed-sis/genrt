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
with userspace, shell, reboot, and power-off contract initramfs images. Those
images reuse the same production ELF files later staged into the release image.
The Raspberry Pi kernel is then built separately, with the exact verified
production initramfs reused from the QEMU release. No userspace is rebuilt
after its contracts. Pi boot autonomy, production composition, ARM64 header,
DTB, FIT component identity/CRC32, and repeated serialization are checked;
`dist` does not perform a physical-board boot or certify RT latency.

`user/c/programs.toml` is the sole declaration of release executable names,
sources, install paths, and contract roles. Build, release staging, contract
staging, identity comparison, and the tested/structural-only coverage report
are derived from that manifest. Source paths must resolve to regular files
strictly below the repository `user/c` tree; absolute paths, parent traversal,
and symlink escapes are rejected.

`tests/qemu/program-contracts.toml` binds every dynamic product one-to-one to
its protocol case, exact contract install path, argv, and either an expected
exit code or a terminal restart/power-off outcome. xtask generates supervisor C
tables from this plan, so contract reporting is not inferred merely from an ELF
being present in the test image.

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
genrt-raspberrypi-3b-plus-vX.Y.Z.itb
genrt-raspberrypi-3b-plus-vX.Y.Z.manifest.json
SHA256SUMS
```

The archive contains `run.sh`, `genrt-aarch64.elf`, `initramfs.cpio`,
`initramfs.manifest.json`, `manifest.json`, `LICENSE`, and a standalone `RUN.md`.
It includes `qemu-virt.dtb` for one CPU and `qemu-virt-smp2.dtb` through
`qemu-virt-smp4.dtb` for the other supported topologies. The same tested kernel
and userspace serve every topology; generating additional DTBs does not rebuild
either executable set.

After extraction, only a POSIX shell and `qemu-system-aarch64` are required:

```bash
./run.sh             # One CPU
./run.sh -c 4        # Four CPUs and the matching packaged DTB
./run.sh --help
```

The launcher locates artifacts beside itself, rejects invalid options/counts
before launching QEMU, and preserves QEMU's exit status. It is generated from
the same canonical configuration as interactive xtask run, including reset and
power-off behavior. No QEMU argument forwarding or runtime DTB generation is
supported. `RUN.md` provides equivalent direct commands, console/exit guidance,
and troubleshooting without referring to the source checkout.

The bundle manifest records SemVer prerelease status, commit, tool versions,
target/profile/features, and SHA-256 hashes of every payload file, including
the launcher, guide, license, and all DTBs. It excludes its own hash;
`SHA256SUMS` covers the complete archive, including `manifest.json`, plus the
standalone Pi FIT and its sidecar manifest. All three assets are listed in
sorted order. The output directory must contain only these release assets
and `SHA256SUMS`; unrelated entries are rejected, not removed.
One sorted inventory controls manifest coverage and archive membership.
`run.sh` is archived with mode 0755. Archive ordering and metadata are fixed,
and serialization of the same staging tree is repeated and compared.

## Raspberry Pi asset

The standalone `.itb` contains the ordinary, non-profiled Pi kernel, board DTB,
and production initramfs. It is separate from the QEMU archive and does not
require extraction or rebuilding. Verify `SHA256SUMS` before deployment, then
follow the [Pi bootstrap guide](../arch/aarch64/platform/raspberrypi-3b-plus/README.md)
for the required 64-bit FIT-capable U-Boot and firmware. For microSD, copy the
FIT to the existing FAT boot partition as `genrt.itb`; do not use `dd` on the
card. The existing `boot.scr` procedure can launch it automatically, or U-Boot
can load it explicitly:

```text
fatload mmc 0:1 01000000 genrt.itb
bootm 01000000
```

The Pi sidecar records the commit, platform, release profile/features, build
tool versions (including DTC and U-Boot tools), FIT SHA-256, and hashes of the
kernel ELF, DTB, and embedded initramfs. Its verification fields explicitly
distinguish the structurally checked Pi kernel from QEMU-contract-tested
userspace. The Pi and QEMU production initramfs bytes are identical; their
platform kernels are not. Earlier physical RT measurements are not a boot or
latency guarantee for the exact released FIT.

Release hosts now require `mkimage` and `dumpimage` as well as the existing
toolchain and device-tree tools; the dependency installer provides them.

## GitHub workflow

The read-only verification job invokes `cargo xtask dist`; only the dependent
publication job has contents write permission. Prerelease status comes from the
SemVer-derived bundle manifest. Runs for the same tag are serialized. A rerun
repairs a partial draft by uploading only missing assets after verifying every
existing asset byte-for-byte, then checks the complete inventory and
`SHA256SUMS` before publishing. Existing conflicting or unexpected assets are
never overwritten.
