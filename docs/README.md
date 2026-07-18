# Documentation map

Use the narrowest document that owns the question:

| Topic | Owner |
| --- | --- |
| Project landing page and quick start | [`README.md`](../README.md) |
| Current implementation snapshot | [`memory/current-state.md`](../memory/current-state.md) |
| Durable cross-cutting constraints | [`memory/invariants.md`](../memory/invariants.md) |
| Architecture decisions | [`memory/decisions/README.md`](../memory/decisions/README.md) |
| AArch64 boot, MMU, exceptions, IRQ | [`arch/aarch64/README.md`](../arch/aarch64/README.md) |
| Generic kernel subsystem map | [`kernel/README.md`](../kernel/README.md) |
| Memory ownership and VM | [`kernel/src/memory/README.md`](../kernel/src/memory/README.md) |
| Scheduling, time, threads, IPC | [`kernel/src/sched/README.md`](../kernel/src/sched/README.md) |
| Initramfs, ramfs, paths, FDs | [`kernel/src/fs/README.md`](../kernel/src/fs/README.md) |
| Userspace build and ABI | [`user/README.md`](../user/README.md) |
| Build and artifact workflows | [`tools/xtask/README.md`](../tools/xtask/README.md) |
| QEMU contracts | [`testing.md`](testing.md), [`tests/qemu/README.md`](../tests/qemu/README.md) |
| Tagged releases | [`releases.md`](releases.md) |
| Host dependency setup | [`development/setup.md`](development/setup.md) |
| Agent workflow | [`development/agent-workflow.md`](development/agent-workflow.md) |
| Debugging | [`development/debugging.md`](development/debugging.md) |
| Active hardening backlog | [`roadmap/hardening.md`](roadmap/hardening.md) |

Do not create calendar-based status plans. Update project memory and the owning
module documentation when behavior changes.
