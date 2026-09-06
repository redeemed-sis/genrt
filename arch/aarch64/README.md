# AArch64 architecture

This is the active genrt architecture: `aarch64-unknown-none-softfloat` on QEMU
`virt,gic-version=2`. CPU0 owns global initialization; DTB-described secondary
CPUs start through PSCI and enter generic scheduler-owned idle contexts after
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
CPU0 installs its vectors and initializes its local GICC, timer PPI, and
physical timer entirely in `rust_entry` before entering generic kernel boot.
Secondaries clear their local TTBR0 after high entry, install vectors, and
perform the same local GICC/PPI/timer setup before entering generic code. One
generic secondary entry then binds logical identity, consumes that CPU's
preallocated idle slot, and enters its existing saved frame. The frame transfer
leaves the bootstrap stack and unmasks IRQ through the architecture ABI.
Generic code never invokes architecture interrupt initialization or selects
DAIF policy. CPU0 has already preallocated every topology CPU's bounded time
state and scheduler idle capacity before PSCI startup.

The same architecture-local PSCI HVC helper implements system-wide
`SYSTEM_RESET` and `SYSTEM_OFF`. The AArch64 layer owns their function IDs and
maps returned PSCI rejection statuses to the narrow generic power-hook result.
Either operation may be requested on any online CPU; no migration to CPU0 or
secondary-CPU shutdown protocol precedes the terminal firmware call.

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
- Published TTBR0 roots have one immutable logical CPU owner. Activation and
  clear use local `VMALLE1`; staged roots are mapped before first activation and
  need no invalidation. There are no ASIDs or cross-CPU userspace shootdowns.
- Runtime TTBR1 mappings remain shared across CPUs and retain inner-shareable
  broadcast invalidation.
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

GICv2 initialization follows register ownership. CPU0 alone enables the shared
distributor and routes device SPIs, including PL011, to CPU0. Each architecture
entry installs that CPU's vector base, initializes its banked GICC interface,
enables the physical-timer PPI and reserved scheduler SGI, and resets its
`CNTP_*` state before local IRQ delivery. During generic logical registration,
the architecture reads the executing interface's banked `GICD_ITARGETSR`
identity and publishes a unique one-hot logical-to-GIC target binding. Generic
code never interprets GIC CPU-target encoding. Generic scheduler initialization
occurs only after this sequence; the CPU registry retains identity/topology only
and does not duplicate readiness.

Each PE owns its physical timer registers; generic time state therefore keeps
one bounded queue per logical CPU and only the executing owner programs that
timer. Timer IRQ entry resolves the existing logical binding, acknowledges and
EOIs through the executing CPU's GICC interface, and dispatches only that CPU's
queue. The secondary first entry restores its scheduler-owned frame and timer
IRQs can then perform normal local scheduler handoff. UART IRQ remains on CPU0
and wakes stdin waiters without allocation.

Cross-CPU runnable publication uses SGI 1 as a scheduler-only notification.
The architecture translates the logical destination through its immutable GIC
target binding, orders published scheduler state before `GICD_SGIR`, and sends
to exactly one interface. IRQ entry acknowledges the full IAR, lets the generic
scheduler drain target-owned ingress and update the existing return context,
then EOIs that same IAR. The SGI carries no payload and is not a general-purpose
IPI channel.

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
ADR-0028, ADR-0038 through ADR-0041 in
[`memory/decisions/`](../../memory/decisions/README.md).
