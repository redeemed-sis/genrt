# ADR-0025: Automated QEMU testing and tagged releases

## Status

Accepted.

## Context

Diagnostic logs, functional contracts, and release image composition have
different stability requirements. Treating prompts, boot prose, or sample
initramfs files as a machine API makes harmless product changes break CI and
allows a userspace test to report success before its process is reaped.

## Decision

- xtask remains the sole implementation of local/hosted checks, QEMU launch,
  test orchestration, artifact verification, and packaging.
- Test-enabled kernels and test-only userspace use the versioned
  `RS GTRT/1|producer|seq|event|subject|detail` protocol. Production kernel and
  release initramfs contain neither its implementation nor its magic strings.
  Test artifacts carry `GENRT_TEST_ARTIFACT_V1` in a retained
  `.genrt.test_marker` section so artifact classification does not depend on
  how the wire record is assembled at runtime.
- Kernel contracts run with scenario-specific `qemu-test` features. System
  contracts run the byte-identical production kernel with controlled test
  initramfs images.
- Test `/init` supervisors own terminal status and emit `DONE PASS` only after
  `fork`, `execve`, `waitpid`, and exit-status validation.
- Human UART output is retained for diagnosis but ignored for pass/fail.
- Production userspace ELF files are built once and reused in test and release
  images. `user/c/programs.toml` declares their sources, install paths, and
  contract roles. Sources must canonically resolve below `user/c`.
  `tests/qemu/program-contracts.toml` binds every dynamic product one-to-one
  to an exact case, executable path, argv, and exit code; xtask generates the
  supervisor invocation table from it.
- Release initramfs composition is checked structurally against a generated
  manifest; exact-image dynamic shell testing is deferred until genrt has a
  genuine production health interface.
- QEMU serial transport uses a bounded host channel, and step/case/suite
  runtime limits always terminate and reap the child. Drain threads continue to
  EOF, and the complete serial log is parsed again to reject malformed or late
  protocol records.
- Test initramfs entries carry explicit provenance and use the narrow
  `/.__genrt_test__/` namespace. Production policy rejects provenance rather
  than generic directory names or prose containing protocol documentation;
  executable marker sections and protocol code are rejected independently.
- The ordinary merge gate applies production policy to both the linked kernel
  and production initramfs. Test initramfs construction is exposed through a
  separate test-only builder and cannot silently select production output.

## Determinism

Guest test paths are finite and bounded. Kernel protocol emission is
allocation-free. Timing tests assert lower bounds rather than host-sensitive
jitter. Initramfs entries are sorted with controlled metadata and serialized
twice from the same staging tree. Release tar/gzip metadata and DTB seed
properties remain controlled.

## Consequences

Test fixtures and protocol IDs are stable test contracts rather than production
ABI. Release content can evolve through its declarative manifest without
rewriting QEMU scenarios. A boot failure appears as protocol timeout accompanied
by the complete serial transcript.

Hardware-in-the-loop, SMP, latency certification, signing, SBOM, attestation,
and a production health channel remain deferred.
