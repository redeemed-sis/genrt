# ADR-0061: Raspberry Pi FIT release artifacts

## Status

Accepted

## Context

Raspberry Pi 3B+ runs the shared kernel with distinct platform features. Releases
must provide a deployable FIT without rebuilding dynamically tested userspace
or implying that QEMU contracts validate a different platform kernel.

## Decision

`xtask dist` retains the QEMU archive and additionally emits a versioned Pi FIT
and JSON manifest as standalone release assets. The common sorted SHA256SUMS
covers every asset; unrelated output entries cause failure, never deletion.
The existing GitHub workflow publishes the verified directory without adding
build or packaging semantics to YAML.

Build the ordinary Pi kernel with the canonical hardware builder. Reuse the
QEMU release's verified production CPIO and manifest byte-for-byte, rather than
rebuilding userspace after its production contracts. Validate Pi boot autonomy,
production composition, board metadata, ARM64 image, exact FIT component bytes,
CRC32, and deterministic FIT serialization.

The Pi manifest records platform/profile/features, build tools, component
hashes, and verification scope. Its platform kernel is structurally verified;
userspace ELF bytes were tested in QEMU contracts. An exact release FIT is not
automatically booted on a physical board and carries no RT latency guarantee.

## Invariants

- QEMU release executables remain byte-identical to their dynamic contracts.
- Pi and QEMU release initramfs bytes match; platform kernel bytes differ.
- Test or diagnostic artifacts cannot enter ordinary releases.
- Packaging does not access a board or modify firmware/storage.
- Artifact metadata never substitutes emulator results for physical validation.

## Consequences

Users can download and deploy the Pi FIT without a source build. Release hosts
require U-Boot tools in addition to the existing toolchain and device-tree tools.
Hardware-in-the-loop verification remains separate from the release gate.

## Alternatives considered

Putting the Pi FIT only inside a QEMU-named archive obscures direct deployment.
Rebuilding its initramfs after QEMU contracts breaks exact executable identity.
Running the Pi kernel with QEMU virt's GIC/PSCI model is not platform validation.

## Validation

Host tests check exact production initramfs reuse, invalid/test provenance
rejection, sorted checksums, and missing/unexpected release assets. Full `dist`
checks the Pi FIT against its actual inputs and reproduces FIT and QEMU archive
serialization. Validate SHA256SUMS, sidecar hashes, and embedded ramdisk identity
from a locally generated release; physical deployment is not part of this test.

## Related decisions

- [ADR-0025](ADR-0025-automated-qemu-testing-and-tagged-releases.md)
- [ADR-0051](ADR-0051-raspberrypi-runtime-spin-table-and-power.md)
