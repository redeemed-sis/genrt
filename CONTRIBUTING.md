# Contributing to genrt

Thanks for helping improve genrt. Contributions are welcome across kernel and
userspace code, QEMU contracts, documentation, developer tooling, and issue
triage.

genrt is intentionally experimental. The active target is single-core AArch64
QEMU `virt`, and changes are expected to preserve deterministic behavior,
explicit ownership, bounded runtime structures, and architecture boundaries.

## Before starting

1. Search existing issues and pull requests for related work.
2. Open a feature request before a large change, a new subsystem, an ABI change,
   or an architecture decision. Describe the problem and intended scope before
   proposing an implementation.
3. Read the root [`AGENTS.md`](AGENTS.md), the nearest nested `AGENTS.md`, and the
   documentation owned by the subsystem you plan to change.
4. Read [`memory/invariants.md`](memory/invariants.md) and select relevant
   accepted decisions from [`memory/decisions/README.md`](memory/decisions/README.md).

Small documentation fixes and focused test improvements can go directly to a
pull request when their intent is clear.

## Development setup

Supported hosts are Arch Linux x86_64 and Ubuntu 24.04/26.04:

```bash
git clone https://github.com/redeemed-sis/genrt.git
cd genrt
./scripts/setup/install-deps.sh
cargo xtask doctor
```

Boot the production image with:

```bash
cargo xtask run-aarch64
```

See [`docs/development/setup.md`](docs/development/setup.md) for exact packages,
manual setup, and troubleshooting.

## Engineering expectations

- Keep changes focused and avoid unrelated refactoring.
- Do not introduce heap allocation in interrupt context, scheduler core, frame
  handoff, or timed-event dispatch.
- Keep architecture-specific behavior in `arch/`; generic kernel policy belongs
  in `kernel/`.
- Do not change the syscall ABI without an ADR, matching userspace headers, and
  production-program contract coverage.
- Local IRQ and preemption exclusion are single-core mechanisms, not SMP locks.
- Keep test protocols, supervisors, and fixture markers out of production
  artifacts.
- Localize `unsafe` and document the invariant that makes it sound.
- Keep repository documentation and commit messages in English.

The complete rules are in [`AGENTS.md`](AGENTS.md).

## Commits and pull requests

Create a focused branch and use
[Conventional Commits](.agents/standards/commits.md), for example:

```text
fix(kernel): reject stale wait completions
docs(repo): clarify public contribution workflow
```

A pull request should explain:

- the problem and why the change is needed;
- the intended scope and important implementation choices;
- any effect on determinism, interrupt latency, ownership, ABI, or artifacts;
- the exact validation commands run and their outcomes;
- documentation or ADR updates, when applicable.

Draft pull requests are welcome for early design feedback.

## Validation

Run the smallest sufficient gate for the change:

| Scope | Minimum validation |
| --- | --- |
| Documentation only | link/stale-reference audit; `git diff --check` |
| `xtask` | format, tests, clippy, and `cargo xtask check` |
| Kernel/AArch64/userspace | host checks, QEMU contract, post-link checks |
| Cross-cutting or release-sensitive | `cargo xtask ci` |

Report only commands that actually ran. If a check could not run, state the
missing tool or environment constraint in the pull request.

## Bugs, features, and security

Use the structured GitHub issue forms and include the commit or release, host
OS, QEMU version, exact command, reproduction steps, and relevant serial output.
Keep logs focused and remove secrets or machine-specific credentials.

Do not publish exploitable security details in a normal issue. Follow
[`SECURITY.md`](SECURITY.md) instead.

By participating, you agree to follow the
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).
