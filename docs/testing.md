# Automated testing

## Gates

The bootstrap script installs the Ubuntu host packages, including `rustup` when
it is not already available. Rust version and components remain sourced from
`rust-toolchain.toml`. Run the same workflow locally from the repository root:

```bash
scripts/ci/install-ubuntu-deps.sh
cargo xtask doctor
cargo xtask check
cargo xtask test-aarch64
cargo xtask ci
```

`check` covers formatting, xtask tests and clippy, userspace compilation,
initramfs verification, AArch64 linking, and the `.boot.text` post-link
invariant. It applies the production marker policy to both the kernel ELF and
the production initramfs; test image builders use a separate test-only entry
point. `ci` is the canonical local and hosted merge gate.

Cases can be listed or selected explicitly:

```bash
cargo xtask test-aarch64 --list
cargo xtask test-aarch64 --case kernel-contract
cargo xtask test-aarch64 --case shell-contract --timeout-secs 30
```

## Test layers

`kernel-contract` and `user-fault` use kernels compiled with scenario-specific
`qemu-test` features. Their coordinators test scheduler/timing/mailbox behavior
and exact user-fault classification.

`userspace-contract` and `shell-contract` use the production kernel with a
test-only initramfs. `/init` is a supervisor that performs
`fork -> execve -> waitpid`; only the supervisor can report terminal success.
The shell case uses the production shell ELF and test-only helper commands. A
host-generated nonce verifies UART RX, blocking stdin wakeup, argv propagation,
and command execution without depending on prompts or human-readable output.

Dynamic production-program checks are declared in
`tests/qemu/program-contracts.toml`. xtask validates a one-to-one mapping to
`user/c/programs.toml` and generates the exact case/path/argv/exit table
compiled into each supervisor. A passing protocol case therefore names the
specific executable invocation that produced it.

Production boot logs and release initramfs contents are not test APIs. Reaching
the userspace supervisor proves the boot, scheduler, initramfs, ELF, EL0, and
syscall path. A failure before that boundary is reported as protocol timeout,
with the complete serial log retained for diagnosis.

## Guest protocol

Machine records start with ASCII Record Separator (`0x1e`):

```text
<RS>GTRT/1|producer|000001|READY|suite
<RS>GTRT/1|producer|000002|CASE_START|case
<RS>GTRT/1|producer|000003|PASS|case
<RS>GTRT/1|producer|000004|DONE|suite|PASS
```

Supported events are `READY`, `CASE_START`, `PASS`, `FAIL`, `DONE`, and
`ABORT`. Sequence numbers are validated independently per producer. Only the
configured supervisor may emit `READY` and the single terminal `DONE PASS`.
Malformed records, sequence gaps, unknown versions, `FAIL`, and `ABORT` fail a
case immediately. Case records follow `unseen -> CASE_START -> PASS`; duplicate
or reordered records fail unless a future DSL action explicitly declares an
unordered concurrent group. All ordinary UART lines are ignored by protocol
evaluation.

TOML steps send input, expect structured records, or issue a nonce challenge.
Every case automatically waits for supervisor `READY` before its steps and for
terminal `DONE PASS` afterward.

## Runner behavior

QEMU uses `-display none -monitor none -nic none -serial stdio`. Serial and
stderr are drained concurrently; the parser receives serial chunks through a
bounded channel and handles records split across reads. Step and case deadlines
plus an aggregate QEMU-runtime budget are enforced, and QEMU is always killed
and reaped. Compilation and image preparation are intentionally outside this
runtime budget and remain governed by the outer CI job timeout.
Setup failures abort the command directly and may occur before a per-case
`result.json` exists; hosted CI still preserves command output for that class
of infrastructure failure.

Each case writes `serial.log`, `qemu-stderr.log`, and `result.json` below
`target/test-results/<case>/`; the suite writes `summary.json`. Failure output
includes the serial tail and full log path.

Test protocol implementations carry `GENRT_TEST_ARTIFACT_V1` in a retained
`.genrt.test_marker` ELF section. Production verification rejects that section
or marker in the kernel and executable initramfs entries, rejects protocol code
as defense in depth, and also enforces provenance. Test files live below the reserved
`/.__genrt_test__/` namespace and carry explicit fixture/supervisor provenance
in generated manifests; ordinary names such as `/test` and `/fixtures` remain
available to products.
