# ADR-0039: Per-CPU interrupt and timer readiness

## Status

Accepted

## Context

ADR-0038 brought every DTB-described CPU through PSCI, high-half MMU entry,
generic logical registration, and a scheduler-offline masked park. ADR-0037
already assigned one bounded deadline queue and physical timer to each logical
CPU, but only CPU0 initialized the GIC CPU interface, timer PPI, architecture
timer state, and generic queue used by the IRQ path.

Scheduler activation is a separate milestone. Before it can be attempted, each
registered CPU needs a complete architecture-local exception and timer path
that can receive, attribute, acknowledge, and finish an interrupt without
mistaking interrupt readiness for scheduler readiness.

## Decision

- AArch64 GICv2 initialization is split by ownership. CPU0 alone initializes
  the shared distributor and routes device SPIs. Every CPU initializes its
  banked GICC interface and enables/configures its own physical-timer PPI.
- Every low entry masks asynchronous exceptions. CPU0's AArch64 `rust_entry`
  installs `VBAR_EL1` and initializes its local GICC/PPI/physical-timer state
  before entering generic `kernel_main`. Generic CPU0 boot then publishes
  fixed-capacity scheduler storage and preallocates every topology CPU's time
  queue before PSCI startup.
- High secondary entry installs that CPU's `VBAR_EL1`, calls a narrow generic
  prepare stage to bind the logical `CpuId`, initializes local GICC/PPI/timer
  state in AArch64, and calls generic completion before entering park. Generic
  code does not invoke architecture interrupt setup or select DAIF policy.
- `kernel::time` owns one preallocated queue per logical CPU. CPU0 reserves and
  publishes all selected queues during boot task context, before secondary
  startup or local IRQ delivery.
  Architecture timer initialization disables `CNTP_CTL_EL0`, moves the compare
  value beyond stale boot deadlines, and verifies the known disabled state.
- CPU0 publishes scheduler storage before secondary startup, but only CPU0's
  scheduler context is initialized and later enters a running thread.
  Secondary time queues and IRQ paths may exist while their scheduler contexts
  remain offline.
- The secondary's parked publication is accepted only after logical identity,
  generic time state, vectors, GICC, timer PPI, and physical timer state are
  complete. The generic registry does not duplicate that ordering contract in
  a separate architecture-readiness flag. Parked and scheduler-online remain
  distinct generic states.
- An interrupt-ready secondary enters an architecture-owned `WFI` loop with IRQ
  unmasked and FIQ, SError, and debug exceptions masked. A failed secondary
  retains the fully masked terminal park and makes boot fail rather than
  publishing partial readiness.
- IRQ entry resolves the executing logical CPU through the existing O(1)
  architecture binding. GICC acknowledge/EOI and `CNTP_*` access are local to
  that CPU, and timer dispatch selects only that CPU's generic time state.
  Scheduler dispatch on an offline secondary is a bounded no-handoff path.
- UART and other device SPIs remain routed to CPU0. This decision adds no IRQ
  affinity API, IPI, remote reschedule, runnable secondary thread, or userspace
  execution on secondary CPUs.

## Invariants

- Shared GIC distributor and SPI policy are initialized only by CPU0; GICC,
  SGI/PPI enable state, VBAR, and physical timer registers are CPU-local.
- Local IRQ cannot be unmasked until logical identity, generic time ownership,
  vectors, GICC, timer PPI, and physical timer state are all ready. CPU0 may
  initialize its local hardware before generic ownership publication only while
  IRQ remains masked.
- Architecture code owns local interrupt initialization and park masking.
  Generic prepare/completion calls own only logical registration and parked
  publication; `parked` and `scheduler_online` remain distinct states.
- Every timer IRQ is attributed to the executing `CpuId`, touches only that
  CPU's deadline queue and timer registers, and completes GICC EOI before the
  test or idle path observes completion.
- Per-CPU queue capacity is fixed on CPU0 before secondary startup. Secondary
  bring-up, timer and interrupt initialization, dispatch, diagnostics, and park
  loops allocate nothing and perform bounded work.

## Consequences

All registered CPUs now have complete local exception, GIC, physical timer, and
generic time ownership before IRQ-enabled idle. Issue #6 can therefore focus on
constructing and activating secondary scheduler contexts rather than repeating
architecture interrupt bring-up.

CPU0 and secondary startup are intentionally symmetric at the architecture
boundary: both architecture entries initialize their own local hardware. The
secondary alone needs generic prepare/completion calls because CPU0 must
coordinate its registration and topology-dependent queue publication before
PSCI startup. This avoids architecture setup callbacks, architecture-ready
flags, and DAIF policy parameters in generic kernel code.

CPU0 still owns all normal threads, userspace, device IRQs, and scheduling.
There is no IPI command path, remote timer insertion, migration, or partial CPU
availability policy. The scheduler is published earlier in CPU0 boot so an
offline secondary timer IRQ can safely query scheduler state, but publication
does not make any secondary scheduler context online.

This decision partially supersedes ADR-0038's permanently masked `WFE` park and
secondary timer/interrupt inactivity boundary. It preserves ADR-0038's PSCI,
stack, MMU, registration, and scheduler-offline ownership decisions, and uses
ADR-0037's per-CPU deadline model unchanged.

## Alternatives considered

- Keep secondary IRQ masked until scheduler activation: rejected because it
  leaves architecture interrupt readiness entangled with scheduler work.
- Initialize one global timer queue: rejected because physical timers and the
  existing time ownership model are CPU-local.
- Enable device SPIs on all CPUs: rejected because device affinity and remote
  wake policy are outside this milestone.
- Mark a CPU scheduler-online when its timer works: rejected because exception
  readiness provides no idle thread, current context, ready policy, or IPI
  protocol.
- Poll timer state from the park loop: rejected because it would not validate
  real GIC acknowledge/EOI and interrupt return.

## Validation

- Host tests cover fixed per-CPU time storage and logical CPU identity.
- The one-CPU `kernel-contract` verifies the existing scheduler and timer path
  after the global/local GIC split.
- The four-CPU `smp-boot` contract validates architecture readiness, CPU0-only
  UART SPI routing, offline secondary scheduler contexts, and two completed
  local physical-timer acknowledge/EOI cycles on every logical CPU. The second
  deadline proves return to IRQ-enabled idle and local PPI retrigger after EOI.
- The post-link boot autonomy check still covers both low entries.
- Canonical cross-cutting verification is `cargo xtask ci`.

## Related decisions

- [ADR-0002](ADR-0002-aarch64-irq-path-gicv2-timer.md)
- [ADR-0005](ADR-0005-one-shot-timer-deadline-engine.md)
- [ADR-0035](ADR-0035-logical-cpu-identity-and-per-cpu-scheduler-context.md)
- [ADR-0036](ADR-0036-smp-safe-runtime-synchronization.md)
- [ADR-0037](ADR-0037-per-cpu-deadline-queues-and-direct-scheduler-dispatch.md)
- [ADR-0038](ADR-0038-secondary-cpu-psci-boot-and-parked-state.md)
