# ADR-0058: Bounded sleep and VM-owned usercopy

## Status

Accepted

Refines the implementations of ADR-0032, ADR-0054, and ADR-0028 without changing
wait identity, installed-root lifetime, syscall ABI, or saved-frame layout.

## Context

Periodic userspace sleeps repeatedly acquire scheduler ownership for wait
preparation and commit. Usercopy also acquires it merely to recover the root
already installed on the current CPU. AArch64 full-frame assignments lower to
generic memory-copy calls despite their fixed, disjoint 280-byte payloads.

Idle-root retention means that installed TTBR0 alone cannot authorize usercopy:
trusted idle code must remain ineligible even while preserving that root.

## Decision

- Hold one scheduler guard and current-CPU view across automatic EL0 sleep
  preparation, exact timed-event publication, and commit. Release the nested
  time owner before context selection. Validation and usercopy precede entry.
- Keep other condition-owner protocols unchanged. No external owner lock
  crosses commit; timer IRQ extraction still releases time ownership before
  scheduler acquisition. Preserve generation/sequence and first-completion rules.
- VM owns both the installed-root lifetime record and CPU-local usercopy
  eligibility. User activation publishes eligibility after successful root
  installation, including same-root resumes. Idle revokes eligibility while
  retaining installed lifetime; detach revokes it before hardware clearing.
- Resolve usercopy root identity directly from this O(1) CPU-local publication.
  The snapshot is used only in a stable syscall/trap context, not retained
  across a handoff. Keep full range, page-permission, and EFAULT validation.
- Use fixed 64-byte-aligned CPU publication slots. Correctness does not depend
  on a hardware cache-line size or runtime cache geometry.
- Copy the AArch64 payload through fixed general-purpose load/store pairs and
  a final word, without generic memcpy/memmove or FP/SIMD. Preserve the 280-byte
  frame, 288-byte storage/stack footprint, C hooks, and complete idle saving.

## Invariants

- No heap allocation, usercopy, or external condition callback enters the sleep
  transaction.
- Installed roots cannot be destroyed merely because user access is revoked.
- Idle and other kernel contexts do not gain user-pointer capabilities.
- Failed activation/detach does not falsely release the installed-root record
  or publish a newly usable user context.
- All frame payload words are transferred; alignment tail and live exception
  padding remain untouched. Generic scheduler code never inspects frame layout.

## Consequences

The hot path removes repeated ownership acquisition and generic frame-copy
dispatch without weakening validation or changing scheduling policy. CPU-local
VM publication has bounded inline storage. Instrumented timing still includes
profiler overhead; hardware latency improvement requires matched board runs.
The combined transaction extends one continuous global scheduler hold across
bounded timer publication instead of releasing and reacquiring it. Lock-hold
time and cross-CPU contention still need measurement; fewer acquisitions alone
do not establish a lower worst-case blocking bound.

## Alternatives considered

- Use installed root alone for usercopy: would incorrectly authorize idle.
- Cache a process pointer in scheduler state: violates process separation.
- Skip saving idle: requires a new restartable-idle contract, not assumed here.
- Reduce register preservation or add a new sleep syscall: changes ABI and is
  unnecessary for these structural optimizations.

## Validation

Host tests check root/eligibility publication and retention. The kernel QEMU
contract checks full frame transfer and untouched guards/padding, selected
context eligibility, repeated user/idle sleeps, exact root-write counts, and
frame return after reap. Production contracts cover usercopy errors, exec,
fork, fault cleanup, and SMP sleeps. Diagnostic full/focused contracts retain
exact wait and phase behavior. Inspect optimized copy hooks for fixed GP
transfers and absence of generic copy calls. No QEMU timing is a Pi speedup.

## Related decisions

- [ADR-0028](ADR-0028-typed-saved-context-and-scheduler-ownership.md)
- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [ADR-0054](ADR-0054-idle-user-root-retention-and-detach.md)
- [ADR-0055](ADR-0055-batched-timer-wakeup-and-idle-selection.md)
- [Profiling guide](../../docs/wake-profiling.md)
