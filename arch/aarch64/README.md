# AArch64 architecture

This is the active genrt architecture: `aarch64-unknown-none-softfloat` on QEMU
`virt,gic-version=2`. CPU0 owns global initialization and scheduling;
DTB-described secondary CPUs can be started through PSCI and parked after
high-half registration.

## Boot sequence

The kernel is loaded at physical `0x4008_0000`. CPU0 enters the low-linked
`_start` trampoline, uses bootstrap stack slot zero, parses the QEMU-loaded DTB,
builds bootstrap TTBR0/TTBR1 tables, programs EL1 translation state, enables
the MMU, switches to a high stack alias, and enters high-linked Rust. CPU0 then
performs all global platform, memory, filesystem, and scheduler initialization.

High Rust parses enabled `/cpus` nodes and the DTB-selected PSCI HVC conduit
into immutable AArch64 topology. After allocator-owned runtime TTBR1 tables are
active, CPU0 starts each secondary sequentially with PSCI `CPU_ON`. The low
`secondary_start` entry receives only an architecture bootstrap-stack slot,
uses a distinct fixed 32 KiB stack, installs boot TTBR0 plus CPU0-published
runtime TTBR1, enables the MMU, and branches to `secondary_rust_entry` in the
high half. It does not clear BSS or repeat global initialization.

The AArch64 facade alone reads `MPIDR_EL1`, normalizes Aff3:Aff0 into an opaque
hardware key, and exposes CPU-local logical binding primitives. The generic
kernel registers CPU0 and every secondary, assigns logical IDs, and verifies
that each PSCI target matches the executing hardware identity. Registration
stores the logical index plus one in `TPIDR_EL1`, so runtime current-CPU
resolution is an O(1) register read and zero remains an explicit unbound state.
Secondaries clear their local TTBR0 after high entry, publish parked readiness,
and enter the architecture-owned `WFE` loop with asynchronous exceptions
masked. Their scheduler contexts remain offline.

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

`xtask` generates a QEMU `virt` DTB for the requested bounded CPU count and
loads it into a reserved platform slot. The low parser extracts only RAM,
PL011, and GICv2 ranges needed for initial mappings. High Rust additionally
parses enabled CPU identities and PSCI metadata before generic boot entry. The
QEMU platform module owns a documented emergency fallback for early UART
diagnostics; generic kernel code does not own platform constants.

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

GICv2 dispatches the architected timer and PL011 RX IRQ. Each PE owns its
physical timer registers; generic time state therefore keeps one bounded queue
per logical CPU and only the executing owner programs that timer. Expiry enters
the generic time path, which invokes the scheduler facade after releasing queue
ownership and may replace the return frame. UART IRQ drains a bounded RX FIFO
path and wakes stdin waiters without allocation.

## Build invariants

- Rust `TrapFrame` layout and assembly offsets change together.
- `SavedContext` storage must fit and align `TrapFrame`; generic scheduler code
  must not cast or inspect either representation.
- Kernel code does not own FP/SIMD state; the soft-float target prevents
  implicit assumptions.
- MMIO accesses use documented Device mappings and localized volatile `unsafe`.
- Boot, linker, exception, MMU, and IRQ changes require post-link verification
  and the relevant QEMU contracts.
- Bootstrap-stack capacity, generic `KERNEL_CPU_CAPACITY`, and accepted QEMU
  CPU count must agree; CPU topology and runtime TTBR1 handoff are published
  with Release/Acquire ordering before secondary use.

Related decisions: ADR-0002 through ADR-0004, ADR-0008, ADR-0015, ADR-0027,
ADR-0028, and ADR-0038 in
[`memory/decisions/`](../../memory/decisions/README.md).
