# AArch64 architecture

This is the active genrt architecture: `aarch64-unknown-none-softfloat` on
single-core QEMU `virt,gic-version=2`.

## Boot sequence

The kernel is loaded at physical `0x4008_0000`. A low-linked trampoline in
`.boot.*` parks secondary CPUs, establishes a boot stack, parses the
QEMU-loaded DTB, builds bootstrap TTBR0/TTBR1 tables, programs EL1 translation
state, enables the MMU, switches to a high stack alias, and enters high-linked
Rust.

Main kernel sections have high VMAs and low LMAs through linker `AT(...)`; no
runtime section copy is required. The address convention is:

```text
KERNEL_HVA_OFFSET = 0xffff_0000_0000_0000
HVA = PA + KERNEL_HVA_OFFSET
```

All code and data reachable before MMU enable must remain in `.boot.text`,
`.boot.rodata`, or `.boot.bss`. The post-link check rejects relocations, runtime
helper thunks, high-VA instruction operands, and direct branches outside that
closed world.

## Platform data

`xtask` generates the QEMU `virt` DTB and loads it into a reserved platform
slot. The low parser extracts only RAM, PL011, and GICv2 ranges needed for
initial mappings. The QEMU platform module owns a documented emergency fallback
for early UART diagnostics; generic kernel code does not own platform constants.

## MMU ownership

- Bootstrap TTBR0 provides temporary identity mappings.
- TTBR1 provides high-half RAM and Device mappings.
- After physical memory initialization, allocator-owned runtime TTBR1 tables
  replace boot tables and TTBR0 is cleared.
- The generic VM layer requests architecture operations through narrow C ABI
  hooks; descriptor and system-register details stay here.

## Exceptions, IRQ, and task return

The vector table saves the full trap frame before using interrupted GPRs.
Current-EL SVC handles kernel task calls; lower-EL SVC dispatches userspace
syscalls. Lower-EL frames save `SP_EL0` while retaining a valid EL1 kernel stack.
Restore selects EL1 or EL0 from SPSR mode bits and returns with `eret`.

Rust exception entry wraps each live `TrapFrame` once as the generic kernel's
opaque `ActiveContext`. The AArch64 context adapter alone decodes `x8` and
`x0..x5` into `SyscallRequest`, stores syscall results in `x0`, rewinds `ELR_EL1`
for restartable SVC, and replaces EL0 state after exec while preserving
`kernel_sp`. Scheduler slots own opaque inline `SavedContext` values. The
AArch64 adapter alone interprets their storage as `TrapFrame`, initializes
kernel/user/fork entry state, and performs bounded live-to-saved transfers.
Compile-time checks require the architecture frame to fit with compatible
alignment; the assembly restore layout remains unchanged.

GICv2 dispatches the architected timer and PL011 RX IRQ. Timer expiry enters the
generic time/scheduler path and may replace the return frame. UART IRQ drains a
bounded RX FIFO path and wakes stdin waiters without allocation.

## Build invariants

- Rust `TrapFrame` layout and assembly offsets change together.
- `SavedContext` storage must fit and align `TrapFrame`; generic scheduler code
  must not cast or inspect either representation.
- Kernel code does not own FP/SIMD state; the soft-float target prevents
  implicit assumptions.
- MMIO accesses use documented Device mappings and localized volatile `unsafe`.
- Boot, linker, exception, MMU, and IRQ changes require post-link verification
  and the relevant QEMU contracts.

Related decisions: ADR-0002 through ADR-0004, ADR-0008, ADR-0015, ADR-0027, and
ADR-0028 in
[`memory/decisions/`](../../memory/decisions/README.md).
