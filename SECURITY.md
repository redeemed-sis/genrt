# Security policy

## Supported versions

genrt is an experimental research operating system and is not supported for
production use. There is no security-maintenance SLA. Accepted fixes target the
`main` branch and may be included in a later prerelease.

## Reporting a vulnerability

Do not open a public issue containing exploit details, secrets, or a proof of
concept that could harm other users.

Use GitHub's private **Report a vulnerability** flow on the repository Security
page when it is available. If private reporting is unavailable, open a minimal
public issue asking the repository owner to establish a private channel; include
no sensitive technical details in that issue.

A useful private report contains:

- the affected commit or release;
- the affected component and security impact;
- reproducible steps or a minimal proof of concept;
- required host, QEMU, or guest configuration;
- suggested mitigations, if known;
- whether and where the issue has already been disclosed.

The maintainer will acknowledge reports and coordinate next steps on a
best-effort basis. Please allow time for validation in the controlled AArch64
QEMU environment before public disclosure.

## Scope

Reports may cover the kernel, userspace ABI, initramfs and ELF loading, build and
release tooling, QEMU test isolation, or release artifact integrity. General
feature gaps documented as current project boundaries are not vulnerabilities
by themselves.
