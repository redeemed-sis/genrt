# ADR-0038: Secondary CPU PSCI boot and parked state

## Status

Accepted

## Context

ADR-0035 introduced logical CPU identity and fixed per-CPU scheduler contexts,
and ADR-0036 made shared runtime owners safe for concurrent CPU access. The
active AArch64 target still executed only CPU0: no secondary reached the MMU,
high-half Rust, generic CPU registry, or an observable kernel-owned readiness
state.

QEMU `virt` describes enabled processing elements and PSCI in its DTB. With the
current bare-metal `-kernel` protocol, CPU0 starts at the kernel entry while
secondary CPUs remain powered off until PSCI `CPU_ON`. The kernel therefore
needs a bounded architecture startup path without assigning logical identity in
assembly or duplicating global initialization.

## Decision

- CPU0 parses enabled `/cpus` children and `/psci` from the resident runtime
  DTB in high Rust. AArch64 publishes an immutable CPU0-first topology containing
  raw PSCI targets, normalized hardware identities, the enabled count, and the
  HVC CPU_ON function ID. Generic `BootInfo` independently carries the enabled
  count, and boot rejects disagreement between both parsers.
- `KERNEL_CPU_CAPACITY`, the accepted QEMU `--cpus` range, the AArch64 topology
  array, and linker-owned bootstrap-stack count are fixed at four. CPU0 validates
  this agreement before starting a secondary. Every CPU owns one distinct 32
  KiB bootstrap stack selected by an architecture bootstrap slot; that slot is
  not a generic `CpuId`.
- CPU0 performs all BSS, platform, GIC distributor, memory, runtime TTBR1,
  initramfs, and scheduler initialization. After runtime TTBR1 is installed,
  CPU0 publishes its root with Release ordering and starts each secondary
  sequentially through PSCI `CPU_ON`, passing the selected bootstrap slot as
  `context_id`.
- A secondary enters low-linked `secondary_start`, keeps asynchronous
  exceptions masked, validates and initializes only its private stack, installs
  boot TTBR0 plus the published runtime TTBR1, enables MMU/caches, switches to
  the high stack alias, and calls the architecture secondary Rust entry. High
  entry installs the vector base and clears only that CPU's TTBR0 before
  crossing into generic kernel code.
- `kernel_secondary_main` registers the executing hardware identity in the
  existing generic bounded registry, binds and verifies its logical `CpuId`,
  and publishes parked readiness. CPU0 remains logical CPU0; secondaries receive
  subsequent IDs from the CPU0-selected topology order. Generic code never
  interprets MPIDR or invokes PSCI directly.
- Secondary startup and readiness are allocation-free and independent of the
  scheduler. CPU0 waits a bounded time for each exact secondary to register and
  park. Duplicate identity, topology/capacity disagreement, PSCI failure,
  registration failure, or timeout terminates boot through the existing fatal
  path rather than silently reducing the active topology.
- A successfully registered secondary enters an architecture-owned `WFE` park
  loop. It runs no scheduler context, thread, userspace, normal interrupt, or
  timer work. Its preallocated scheduler context remains uninitialized and
  offline.
- `xtask` accepts a bounded CPU count for AArch64 build/run/debug/QEMU command
  generation, creates a topology-specific DTB, and passes the same value to
  QEMU `-smp`. Ordinary cases default to one CPU. A dedicated four-CPU
  `smp-boot` contract validates the parked-secondary boundary.

## Invariants

- CPU0 is the only owner of global initialization and the only online scheduler
  context in this milestone.
- No two CPUs use the same bootstrap stack, hardware identity, or logical ID.
- Runtime TTBR1 and immutable topology publication happen-before secondary use.
- Logical CPU registration stays in generic kernel code; architecture code owns
  hardware identity, PSCI, MMU transition, stack selection, and terminal park.
- A secondary cannot be considered ready until `current_id()` verifies its
  installed runtime binding and parked state is Release-published.
- Secondary bring-up and park contain no heap allocation, scheduler handoff,
  userspace access, or unbounded runtime container growth.

## Consequences

All DTB-described CPUs now reach a known generic-kernel state, so future work
can initialize local GIC/timer and scheduler ownership without redesigning the
reset-to-high-half path. Normal execution remains CPU0-only. The fixed stack and
topology capacity remains a controlled QEMU-platform bound, and startup failure
is fatal rather than supporting partial CPU availability or hotplug.

This decision partially supersedes ADR-0035's CPU0-only registration and
secondary-context inactivity boundary. It preserves ADR-0035's logical identity,
affinity, and scheduler ownership model, ADR-0036's synchronization model, and
ADR-0037's per-CPU deadline ownership.

## Alternatives considered

- Let every QEMU CPU enter `_start` and elect CPU0: rejected because the
  platform DTB declares PSCI startup and QEMU keeps secondaries powered off.
- Derive logical IDs or stack indices directly from MPIDR: rejected because
  hardware affinity values are architecture identifiers, not bounded generic
  storage indices.
- Start all secondaries concurrently: deferred because sequential CPU0 startup
  gives deterministic logical registration and simpler failure attribution;
  the runtime contract does not depend on QEMU reset ordering.
- Initialize secondary scheduler, GIC CPU interface, and timers now: rejected
  because activation, IPI, and remote scheduling are separate ownership
  milestones.
- Continue parking secondaries in low assembly: rejected because it provides no
  generic identity, high-half readiness, or auditable activation boundary.

## Validation

- Host tests cover bounded CPU registry capacity, duplicate identities, and
  registration state.
- The existing one-CPU QEMU contracts retain their default topology.
- `smp-boot` runs with four CPUs and validates CPU0 identity, enabled and
  registered counts, unique hardware identities and bootstrap stacks, verified
  logical bindings, parked readiness, and offline secondary scheduler contexts.
- The post-link boot autonomy check covers both low entry paths and rejects
  relocations or runtime dependencies outside `.boot.*`.
- Canonical cross-cutting verification is `cargo xtask ci`.

## Related decisions

- [ADR-0001](ADR-0001-architecture-strategy.md)
- [ADR-0015](ADR-0015-aarch64-high-half-mmu-bring-up.md)
- [ADR-0035](ADR-0035-logical-cpu-identity-and-per-cpu-scheduler-context.md)
- [ADR-0036](ADR-0036-smp-safe-runtime-synchronization.md)
- [ADR-0037](ADR-0037-per-cpu-deadline-queues-and-direct-scheduler-dispatch.md)
