# xtask instructions

- `xtask` is the sole implementation of build, QEMU, test, initramfs, and
  release workflow semantics. Keep GitHub Actions as thin command delegates.
- Resolve paths canonically, reject traversal/symlink escapes at trust
  boundaries, and keep child-process ownership explicit through wait, timeout,
  termination, and reap.
- Artifacts must be deterministic: sorted inputs, controlled metadata, tracked
  toolchain/dependencies, exact hashes, and no rebuild after dynamic testing.
- Keep production and test-only builders distinct. Production verification must
  reject protocol code, marker payloads/sections, and test provenance.
- QEMU parsing and transcript retention are bounded; case and suite deadlines
  must terminate and reap QEMU.
- Add unit tests for parser, manifest, source-policy, and negative validation
  paths. Run format, xtask tests, clippy with `-D warnings`, and
  `cargo xtask check`; release-sensitive changes also require `ci` and `dist`.
