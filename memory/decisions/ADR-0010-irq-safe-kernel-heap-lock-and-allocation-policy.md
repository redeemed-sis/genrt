# ADR-0010: IRQ-Safe Kernel Heap Lock and Allocation Policy

## Status

Accepted

## Context

`genrt` already has a fixed-size bootstrap heap built on top of one contiguous
range allocated from the physical frame allocator. The next milestones want to
move more kernel-owned data structures onto `alloc`-backed containers.

On the current single-core system, a task-context allocation can be interrupted
by the timer IRQ. Without additional protection, that IRQ could re-enter the
allocator and corrupt heap state.

At the same time, the project must not create the false impression that heap
allocation is now supported inside IRQ and scheduler fast paths.

## Decision

The kernel heap now uses the shared kernel IRQ-save lock abstraction around
`linked_list_allocator::Heap`.

Key points:

* ordinary task-context `alloc/free` is protected against local IRQ reentrancy
* entering the allocator saves the current local DAIF state, masks IRQs, takes
  the raw lock, and restores the saved IRQ state on exit
* the current implementation is intentionally single-core, while the shared
  abstraction is the intended upgrade point for a future SMP spin acquisition
* the allocator still follows the existing panic path on OOM

Allocation policy for the current kernel stage:

Allowed:

* heap allocation/free during bootstrap and initialization
* heap allocation/free in ordinary task context

Forbidden / not a supported design target:

* heap allocation/free in timer IRQ paths
* heap allocation/free in scheduler frame handoff paths
* heap allocation/free in `kernel::time` fast IRQ dispatch
* heap allocation/free in synchronous/exception fast paths
* heap allocation/free in high-frequency trace paths

Those paths must continue to operate on preallocated or otherwise bounded
structures.

## Consequences

Positive:

* removes the main local IRQ reentrancy hazard for task-context allocation
* keeps the implementation small and easy to audit
* preserves the current bootstrap heap and frame allocator design
* prepares the ground for heap-backed scheduler/time structures outside IRQ fast paths

Limitations:

* still single-core only
* the current no-SMP implementation is not yet an SMP-safe synchronization primitive
* does not make heap allocation acceptable inside IRQ-critical paths
* heap remains fixed-size and non-growable
