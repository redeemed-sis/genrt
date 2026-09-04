# ADR-0041: Targeted scheduler IPI and remote wakeup

## Status

Accepted

## Context

ADR-0040 activated one scheduler context on every registered CPU and introduced
a bounded `remote_ready` ingress for immutable home-CPU placement. Without an
inter-processor notification, every current thread retained an owner-local RR
deadline even when it had no runnable peer. The timer interrupt periodically
drained ingress, bounding eventual pickup but delaying idle wakeup and spending
interrupt work solely on polling.

Removing that fallback also removes the incidental later scheduler entry that
finalizes a just-exited thread's retired kernel stack. Immediate remote wakeup
and post-exit reclamation therefore need one explicit scheduler checkpoint
mechanism that preserves existing lifecycle, affinity, and context ownership.

GICv2 `GICD_SGIR.CPUTargetList` names CPU interfaces with architecture target
bits. These bits are not generic logical CPU indexes. The banked read-only
`GICD_ITARGETSR0..7` fields report the executing CPU interface's target bit in a
multiprocessor implementation.

## Decision

- Reserve SGI 1 for scheduler notification. Every AArch64 CPU enables and
  prioritizes it with other banked private interrupt state before scheduler
  entry.
- During logical CPU binding, AArch64 reads the executing interface's banked
  private-interrupt target field and publishes one unique one-hot mapping from
  logical index to GIC target bit. A uniprocessor RAZ/WI implementation may use
  the sole CPU0 target. Generic kernel code never stores or interprets target
  masks.
- Expose only a narrow architecture hook that sends the scheduler SGI to one
  logical CPU. The implementation issues a release barrier and one targeted
  `GICD_SGIR` command write. It does not acquire the distributor configuration
  lock, allocate, broadcast, or carry a payload.
- Keep `remote_ready` as the authoritative bounded list of runnable threads
  transferred to each immutable home CPU. A fixed scheduler-owned boolean per
  CPU represents whether one scheduler notification is outstanding.
- Under the shared IRQ-safe scheduler lock, remote publication performs the
  lifecycle transition to `Ready`, appends the complete generation-bearing
  `ThreadId`, changes the target's notification bit from clear to set, and sends
  an SGI only on that edge. Sending under this bounded critical section closes
  the publication-to-notification window. The architecture send leaf acquires
  no conflicting lock.
- The target IRQ acknowledges the full IAR, enters the generic scheduler with
  its exclusive `ActiveContext`, and EOIs the same IAR afterward. Under
  scheduler ownership, the target clears its notification bit and drains all
  ingress into its owner-local ready queue. It then requests and consumes the
  existing local preemption checkpoint; no IPI-specific context-switch path is
  added.
- Duplicate SGIs with no outstanding work are no-ops. A publisher racing with
  consumption either joins the still-pending batch or observes the cleared bit
  under the same lock and sends a new SGI. Correctness does not depend on the
  number of delivered interrupts.
- Thread exit requests the same coalesced SGI for its executing CPU after
  selecting a replacement and recording the retired slot. The interrupt can be
  handled only after exception return moves execution to the replacement stack,
  making the subsequent retired finalization safe without a polling timer.
- Arm an RR quantum only while the local ready queue contains another runnable
  thread. Idle and sole-current contexts rely on one-shot workload deadlines
  and interrupt-driven remote notification, not scheduler polling.
- Keep public thread, wait, and mailbox APIs unchanged. Every remote spawn,
  wait completion, mailbox wake, join completion, and process wait that reaches
  the centralized remote-ready publication inherits the same notification path.

## Invariants

- Lifecycle state and ingress are visible before SGI generation. The
  scheduler lock jointly owns ingress and the coalescing bit, preventing a
  clear-versus-publish lost wakeup.
- A non-idle `Ready` thread has exactly one generation-matching entry across its
  home CPU's local ready queue and remote ingress. An SGI carries no thread
  identity and cannot create membership by itself.
- Only the home CPU drains its ingress or mutates its local ready queue and
  preemption state. Remote CPUs do not program its timer or switch its context.
- The scheduler SGI is targeted to one architecture CPU-interface bit. Generic
  `CpuId` values, MPIDR affinity values, and GIC target encoding remain distinct
  ownership domains.
- SGI send, receive, ingress drain, preemption request, and IRQ-return handoff
  are bounded and allocation-free. EOI remains outside scheduler ownership.
- Retired stack reclamation occurs only at a later scheduler entry on the owner
  CPU after the replacement context has been installed.

## Consequences

Remote runnable work wakes an idle or busy home CPU immediately through its
normal IRQ path. Multiple publications can share one interrupt, and duplicate
delivery is harmless. Sole-current CPUs no longer retain periodic RR deadlines,
reducing timer traffic and removing ingress polling.

The shared scheduler critical section now contains one bounded architecture
command write when a notification bit changes from clear to set. A target may
briefly wait for that lock after taking the SGI, but the publisher cannot be
preempted locally and performs no allocation or unbounded work.

This is deliberately not a general-purpose IPI framework. Remote timer
commands, TLB shootdown, migration, broadcast, CPU hotplug, and userspace SMP
need separate protocols and ownership decisions.

ADR-0041 supersedes only ADR-0040's no-peer periodic quantum and passive
remote-ingress pickup. ADR-0040's per-CPU scheduler, immutable affinity, and
retired-stack ownership remain in force.

## Alternatives considered

- Encode `CpuId` as `1 << index`: rejected because GIC CPU-interface target
  numbering is architecture state and need not match generic logical order.
- Send an SGI for every remote publication: rejected because explicit
  scheduler-owned coalescing bounds redundant interrupt traffic.
- Set an atomic notification bit outside scheduler ownership: rejected because
  clearing it independently from ingress permits a lost-wakeup race.
- Return a send ticket and notify after dropping the scheduler lock: rejected
  because restoring local IRQ state creates a liveness window before the SGI.
- Keep the periodic no-peer quantum: rejected because it delays idle wakeup and
  turns the timer into a remote-work polling mechanism.
- Reclaim an exiting stack during the exit handoff: rejected because exception
  return still executes on that stack.

## Validation

- Host transition tests cover target-only ingress consumption and coalescing
  state across multiple publications and a later batch.
- The four-CPU `smp-boot` contract covers targeted preemption of a busy CPU,
  wake from idle, absence of a no-peer polling quantum, routing isolation, a
  controlled batch that observes one notification send edge for three remote
  publications, exactly-once execution, cross-CPU mailbox and join completion,
  local timer round-robin, and explicit per-CPU sleep deadlines. Runtime
  correctness still derives from ingress state and does not depend on an exact
  interrupt count.
- `cargo xtask ci` remains the canonical cross-cutting gate.

## Related decisions

- [ADR-0002](ADR-0002-aarch64-irq-path-gicv2-timer.md)
- [ADR-0003](ADR-0003-aarch64-preemptive-irq-return-switching.md)
- [ADR-0030](ADR-0030-nested-preemption-control-and-deferred-rescheduling.md)
- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [ADR-0035](ADR-0035-logical-cpu-identity-and-per-cpu-scheduler-context.md)
- [ADR-0036](ADR-0036-smp-safe-runtime-synchronization.md)
- [ADR-0037](ADR-0037-per-cpu-deadline-queues-and-direct-scheduler-dispatch.md)
- [ADR-0040](ADR-0040-per-cpu-scheduler-activation.md)
- [Arm GIC Architecture Specification v2.0, IHI 0048B.b](https://developer.arm.com/documentation/ihi0048/latest/)
