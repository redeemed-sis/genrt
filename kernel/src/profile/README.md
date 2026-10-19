# Sleep/wakeup profiler

The optional `wake-profile` subsystem observes absolute CLOCK_MONOTONIC
sleeps through the exact-wait lifecycle.
It owns diagnostic records, not wait completion, scheduling, or timer policy.
Other subsystems use only the `crate::profile` facade.

## Modules

| File | Responsibility |
| --- | --- |
| `mod.rs` | Stable reexports and the shared architecture counter hook |
| `format.rs` | V1 layout, phase identifiers, configuration validation, and export byte views |
| `session.rs` | Preallocated records, exact wait correlation, phase capture, and freeze/read accounting |
| `runtime.rs` | Per-CPU leaf locks, START/STOP/DISCARD, task-context heap window, cleanup, and runtime hooks |
| `session/tests.rs` | Session state, layout, capacity, correlation, and frozen export tests |
| `runtime/tests.rs` | Owner lifecycle and IRQ/preemption window tests |

Record formats and generic phase-stamping helpers are private to this subsystem.
The facade exposes named events such as `wait_published`, `sleep_committed`,
`sleep_handoff`, `wake_selected`, and `context_restored`; callers do not select
a `Phase`. Selection uses typed `WakePath::Idle` or `WakePath::Generic`, while
early return is classified during syscall admission. IRQ boundaries likewise
use named hooks. The V1 export phase identifiers and layout remain unchanged.

`wake_selected` is emitted by the transition that commits Running/current,
before invariant validation or context handoff. `context_restored` remains in
handoff code after incoming context restoration and address-space activation.

## Ownership

`runtime` acquires the CPU-local `IrqSpinLock` before mutating a published
session. `session` owns the record lifecycle and does not acquire locks or call
the scheduler. `format` has no runtime state or architecture dependencies.
Counter reads use the shared private hook through the facade.
Focused mode skips unselected Rust timestamp reads/writes, but retains hook
calls, leaf locks, lifecycle bookkeeping, and fixed assembly entry probes.
Absolute-sleep admission consumes the current SVC's staged owner/entry once,
without repeating scheduler identity lookup. IRQs remain masked and no handoff
occurs between staging and admission; exact owner and wait checks remain intact.

Heap allocation and destruction occur in task context outside profiler locks.
The heap window allows IRQ delivery while preventing a scheduler handoff, then
restores the original IRQ state before releasing preemption exclusion.
Active collection stays bounded and allocation-free; export usercopy runs
after the profiler lock has been released.

Frozen reads are consumed by rtbench after STOP to print fixed-size interval
aggregates. No raw records are emitted over UART or analyzed by a host command.

See the [profiling guide](../../../docs/wake-profiling.md) for collection,
console reports, and hardware verification, and
[ADR-0056](../../../memory/decisions/ADR-0056-bounded-sleep-wakeup-phase-profiling.md)
for collection ownership and determinism constraints. Reporting is defined by
[ADR-0057](../../../memory/decisions/ADR-0057-in-process-sleep-profile-reports.md).
