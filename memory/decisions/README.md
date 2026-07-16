# Architecture decision records

Read ADRs selectively by scope. `Accepted` records remain historical contracts;
the supersession columns identify only explicit replacement, not later feature
growth.

| ADR | Title | Status | Scope | Supersedes | Superseded by |
| --- | --- | --- | --- | --- | --- |
| [0001](ADR-0001-architecture-strategy.md) | Start with AArch64 on QEMU virt | Accepted | Architecture strategy | - | - |
| [0002](ADR-0002-aarch64-irq-path-gicv2-timer.md) | AArch64 IRQ path via GICv2 and timer | Accepted | AArch64 IRQ | - | - |
| [0003](ADR-0003-aarch64-preemptive-irq-return-switching.md) | Preemptive IRQ-return switching | Accepted | Scheduler/ABI | - | - |
| [0004](ADR-0004-aarch64-boot-exception-separation-and-fatal-path.md) | Boot/exception separation | Accepted | AArch64 exceptions | - | - |
| [0005](ADR-0005-one-shot-timer-deadline-engine.md) | One-shot timer deadline engine | Accepted | Time | - | - |
| [0006](ADR-0006-time-owned-timed-events.md) | Time-owned timed events | Accepted | Time/scheduler | - | - |
| [0007](ADR-0007-dtb-memory-map-and-frame-allocator.md) | DTB memory map and frame allocator | Accepted | Memory | - | - |
| [0008](ADR-0008-aarch64-softfloat-kernel-target.md) | AArch64 soft-float target | Accepted | Build/ABI | - | - |
| [0009](ADR-0009-bootstrap-kernel-heap-on-frame-allocator.md) | Bootstrap heap on frame allocation | Accepted | Memory | - | - |
| [0010](ADR-0010-irq-safe-kernel-heap-lock-and-allocation-policy.md) | IRQ-safe heap policy | Accepted | Memory/RT | - | [0029](ADR-0029-local-irq-and-task-preemption-exclusion.md) (heap lock ownership only) |
| [0011](ADR-0011-dynamic-preallocated-scheduler-and-time-structures.md) | Preallocated scheduler/time structures | Accepted | Scheduler/time | - | [0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md) (saved-frame backing only) |
| [0012](ADR-0012-bounded-mailbox-ipc.md) | Bounded mailbox IPC | Accepted | IPC | - | [0029](ADR-0029-local-irq-and-task-preemption-exclusion.md) (lock naming only) |
| [0013](ADR-0013-mailbox-timeout-semantics.md) | Mailbox timeout semantics | Accepted | IPC/time | - | - |
| [0014](ADR-0014-bounded-kernel-thread-lifecycle.md) | Bounded kernel thread lifecycle | Accepted | Threads | - | - |
| [0015](ADR-0015-aarch64-high-half-mmu-bring-up.md) | AArch64 high-half MMU | Accepted | AArch64/MMU | - | - |
| [0016](ADR-0016-first-aarch64-el0-process.md) | First AArch64 EL0 process | Accepted | Userspace/MMU | - | - |
| [0017](ADR-0017-process-table-and-user-fault-policy.md) | Process table and user faults | Accepted | Processes | - | - |
| [0018](ADR-0018-userspace-elf-loader.md) | Userspace ELF loader only | Accepted | Loader | - | - |
| [0019](ADR-0019-readonly-ramfs-and-fd-table.md) | Readonly ramfs and FD table | Accepted | FD ABI; original backing partially replaced | - | [0021](ADR-0021-initramfs-cpio-root.md) (backing only) |
| [0020](ADR-0020-uart-stdin-and-shell.md) | UART stdin and shell | Accepted | Console/userspace | - | - |
| [0021](ADR-0021-initramfs-cpio-root.md) | CPIO initramfs root | Accepted | Filesystem backing | [0019](ADR-0019-readonly-ramfs-and-fd-table.md) (backing only) | - |
| [0022](ADR-0022-fork-exec-waitpid-echo.md) | Minimal process control | Accepted | Processes/syscalls | - | - |
| [0023](ADR-0023-directory-fds-and-getdents64.md) | Directory FDs and getdents64 | Accepted | Filesystem ABI | - | - |
| [0024](ADR-0024-process-cwd-and-path-resolution.md) | Process cwd and path traversal | Accepted | Filesystem/process | - | - |
| [0025](ADR-0025-automated-qemu-testing-and-tagged-releases.md) | Automated testing and releases | Accepted | Verification/release | - | - |
| [0026](ADR-0026-agent-oriented-development-workflow.md) | Agent-oriented development workflow | Accepted | Repository workflow | - | - |
| [0027](ADR-0027-typed-active-context-and-syscall-boundary.md) | Typed active context and syscall boundary | Accepted | Kernel/AArch64 context boundary | - | [0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md) (saved-frame bridge only) |
| [0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md) | Typed saved context and scheduler ownership | Accepted | Scheduler/AArch64 context ownership | [0011](ADR-0011-dynamic-preallocated-scheduler-and-time-structures.md) (saved-frame backing), [0027](ADR-0027-typed-active-context-and-syscall-boundary.md) (saved-frame bridge) | - |
| [0029](ADR-0029-local-irq-and-task-preemption-exclusion.md) | Local IRQ and task preemption exclusion | Accepted | Synchronization/memory/RT | [0010](ADR-0010-irq-safe-kernel-heap-lock-and-allocation-policy.md) (heap lock ownership), [0012](ADR-0012-bounded-mailbox-ipc.md) (lock naming) | [0030](ADR-0030-nested-preemption-control-and-deferred-rescheduling.md) (transitional preemption backend only) |
| [0030](ADR-0030-nested-preemption-control-and-deferred-rescheduling.md) | Nested preemption control and deferred rescheduling | Accepted | Synchronization/scheduler/RT | [0029](ADR-0029-local-irq-and-task-preemption-exclusion.md) (transitional preemption backend only) | - |

Use [`TEMPLATE.md`](TEMPLATE.md) for new decisions.
