# ADR-0044: Linux-compatible reboot power control

## Status

Accepted

## Context

genrt userspace can run processes on every online CPU, but it has no production
interface for system reset or power off. AArch64 already uses the DTB-selected
PSCI HVC conduit for secondary `CPU_ON`, while generic kernel and userspace code
intentionally contain no PSCI identifiers or calling convention.

Linux exposes both operations through one `reboot(2)` syscall. Successful power
control is terminal, so the existing QEMU contract rule requiring a returning
supervisor to emit `DONE PASS` cannot represent it without weaker evidence.

## Decision

- Reserve syscall number 13 as `SYS_REBOOT` with the Linux raw argument shape
  `reboot(magic, magic2, cmd, arg)`. Interpret command fields at their Linux
  32-bit UAPI width, accept all standard secondary magic values, and support
  only `LINUX_REBOOT_CMD_RESTART` and `LINUX_REBOOT_CMD_POWER_OFF`.
- Return `EINVAL` for invalid magic or unsupported commands. The fourth
  argument is accepted but unused. Success cannot return; a returned backend
  status becomes `ENOTSUP`, `EPERM`, or `EIO` while the syscall context lives.
- Keep generic policy in `kernel::power` as restart or power-off plus two narrow
  architecture hooks. Generic code knows neither PSCI IDs nor HVC.
- Implement AArch64 operations with PSCI 0.2 `SYSTEM_RESET` and `SYSTEM_OFF`
  through the platform-local HVC helper also used by `CPU_ON`. Require the DTB
  to advertise PSCI 0.2 or 1.0 with the existing HVC conduit.
- Do not migrate the caller, stop CPUs, or introduce a power-control IPI. PSCI
  system operations may be invoked directly from any online CPU.
- Expose `<sys/reboot.h>` with `reboot(int)`, `RB_AUTOBOOT`, and
  `RB_POWER_OFF`. The wrapper owns raw Linux magic. Production `/bin/reboot`
  and `/bin/poweroff` call only this userspace API.
- Until credentials exist, any process with syscall access may request either
  operation. A future permission check belongs between ABI validation and the
  architecture call without changing the userspace interface.
- Add test-only `GTRT/1 TERMINAL` records. A trusted supervisor emits
  `CASE_START` and arms exactly `RESTART` or `POWER_OFF` before executing the
  exact production binary. A terminal record is evidence of intent, not
  success by itself.
- Launch terminal QEMU cases with distinct `reboot=reset` and
  `shutdown=poweroff` actions. Power-off requires natural successful QEMU exit.
  Restart requires a new sequence-one `READY` epoch from the same supervisor in
  the same QEMU process. The new supervisor waits for host input, preventing an
  automatic reboot loop.

## Invariants

- PSCI function IDs, HVC register assignment, and status translation remain in
  the AArch64 platform layer.
- The generic syscall path performs no allocation, lock acquisition, blocking,
  scheduler transition, migration, or CPU shutdown orchestration.
- A returned architecture call is always an error; success is terminal.
- Terminal contract success requires trusted protocol intent and the
  operation-specific QEMU lifecycle outcome.
- Test protocol code and supervisors remain absent from production artifacts.

## Consequences

Freestanding programs can request Linux-shaped restart and power off without
knowing the active architecture. The implementation works from CPU0 and a
secondary CPU because PSCI owns the system-wide transition.

The interface omits halt, restart2, kexec, suspend, filesystem shutdown,
service management, credentials, extra PSCI conduits, and hardware backends.

## Validation

- Kernel unit tests cover accepted commands and secondary magic values,
  invalid magic, unsupported commands, and backend error mapping.
- The production-kernel userspace contract sends invalid raw tuples and checks
  `EINVAL` without terminating QEMU.
- Dedicated contracts execute exact production `/bin/poweroff` and
  `/bin/reboot` ELF files. Restart observes a fresh boot epoch; the SMP variant
  launches `/bin/reboot` on CPU1 through production taskset.
- Release-profile contract images and the final initramfs use the same
  previously built ELF files and compare their manifest hashes.

## Related decisions

- [ADR-0025](ADR-0025-automated-qemu-testing-and-tagged-releases.md)
- [ADR-0027](ADR-0027-typed-active-context-and-syscall-boundary.md)
- [ADR-0038](ADR-0038-secondary-cpu-psci-boot-and-parked-state.md)
- [ADR-0042](ADR-0042-smp-userspace-memory-and-fork-affinity.md)
