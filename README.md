# genrt

**genrt** is an experimental hard real-time operating system project written primarily in **Rust**.

Current active target:

* **AArch64**
* **Rust target `aarch64-unknown-none-softfloat`**
* **QEMU `virt`**
* **single-core EL1 kernel threads**
* **QEMU-first bring-up and debugging**

## Current status

The current AArch64 path already has:

* boot entry in `boot.S`
* `VBAR_EL1` exception-vector setup
* early PL011 UART output
* `BootInfo` handoff into Rust
* DTB-seeded physical memory discovery
* generated-and-embedded QEMU `virt` DTB fallback for ELF boot
* internal physical memory map with reserved-range carving
* page-aligned usable frame ranges
* minimal free-list physical frame allocator
* fixed-size bootstrap kernel heap on `linked_list_allocator`
* single-core IRQ-safe heap lock for task-context allocation/free
* working `alloc` container smoke tests (`Vec`, `VecDeque`, `BinaryHeap`, `BTreeMap`)
* GICv2 initialization
* architected timer in one-shot nearest-deadline mode
* monotonic hardware counter timebase
* full trap-frame save/restore on IRQ
* **IRQ-return-based preemptive task switching**
* heap-backed task table with stable boxed stacks and saved frames
* preallocated heap-backed ready queue for runnable tasks
* round-robin scheduling for runnable kernel tasks
* scheduler ownership isolated to bootstrap, timed-event dispatch, and frame handoff
* `kernel::time` owns a preallocated heap-backed deadline queue and one-shot timer rearming
* sleep wakeups and scheduler quantum both delivered as typed timed events
* round-robin quantum configured as a duration at scheduler bootstrap
* bounded mailbox IPC for kernel tasks with heap-preallocated buffers and wait queues
* demo producer/consumer tasks exchanging messages through a capacity-bounded mailbox
* minimal allocation-free formatted logging with log levels
* improved fatal exception diagnostics

In one sentence:

> genrt is currently an early **single-core preemptive EL1 kernel prototype** on AArch64/QEMU.

The AArch64 build currently uses the Rust target `aarch64-unknown-none-softfloat`.
This is intentional for the current kernel stage: the scheduler/trap path does not
yet own FP/SIMD state, so the build avoids implicit hard-float/AdvSIMD assumptions
in ordinary Rust code.

## What is not implemented yet

* MMU / virtual memory
* EL0 / user mode
* SMP scheduling
* mailbox timeout operations
* mailbox registry / dynamic mailbox creation
* driver model
* low-overhead buffered tracing

## Execution model

High-level flow:

```text
_start (boot.S)
  -> early arch init
  -> GICv2 init
  -> one-shot timer init
  -> BootInfo + DTB memory discovery
  -> kernel_main()
  -> physical memory init
  -> start first task from prepared trap frame

Timer IRQ
  -> save full TrapFrame
  -> identify timer interrupt
  -> kernel::time::on_timer_interrupt(frame)
    -> read monotonic counter
    -> collect all expired timed events
    -> dispatch WakeTask / QuantumExpired
    -> scheduler may select next task
    -> compute nearest next deadline
    -> reprogram one-shot timer
    -> active frame may be replaced
  -> restore selected TrapFrame
  -> eret into selected task
```

Key milestone already reached:

> **task switching is performed by replacing the IRQ return frame, not by a normal function-call-style switch**

## Current limitations

* single-core only
* EL1 kernel threads only
* no MMU
* heap is currently a fixed-size `16 MiB` bootstrap region
* direct-to-UART logging
* scheduler/time dynamic containers are preallocated at bootstrap and must not grow in IRQ paths
* heap does not grow from arbitrary frames yet
* scheduler/task management still in early-kernel form
* platform-specific MMIO mapping still partly lives in the AArch64 layer

## Repository layout

```text
genrt/
├── arch/aarch64/      # AArch64-specific boot, traps, timer, GIC, low-level context handling
├── kernel/            # architecture-neutral kernel logic
├── crates/bootinfo/   # early boot handoff structures
├── tools/xtask/       # build/run/debug workflow
├── docs/
└── ai-docs/
```

## Logging

Available macros:

* `kprint!`, `kprintln!`
* `error!`, `warn!`, `info!`, `debug!`, `trace!`

Available levels:

* `Error`
* `Warn`
* `Info`
* `Debug`
* `Trace`

The logger is allocation-free and intended for kernel bring-up. It is useful for diagnostics, but high-volume UART logging still perturbs timing.

## Heap

The kernel heap is currently initialized from one contiguous `16 MiB` region
allocated out of the physical frame allocator during early memory bootstrap.

Initialization order is:

1. parse and normalize physical memory regions
2. initialize the frame allocator on usable page ranges
3. allocate one contiguous heap range via `alloc_contiguous`
4. initialize `linked_list_allocator`
5. run heap-backed smoke tests

This keeps heap ownership unambiguous: once the bootstrap heap region is
allocated, it is no longer part of the frame allocator free list.

Allocation policy for the current kernel stage:

* heap allocation/free is allowed during bootstrap and in ordinary task context
* heap allocation/free is protected against local IRQ reentrancy on the current single core
* heap allocation/free remains forbidden in timer IRQ, scheduler handoff, time fast-path dispatch, exception fast paths, and high-frequency tracing
* dynamic containers used by those IRQ-critical paths must be preallocated or otherwise bounded before entering the fast path

The scheduler and time subsystem now follow that rule explicitly:

* the task table, saved frames, task stacks, ready queue, and deadline queue are heap-backed
* all of those containers are allocated and reserved during bootstrap
* timer IRQ and scheduler handoff only perform bounded operations on already allocated storage

## IPC

The first IPC primitive is a bounded mailbox for EL1 kernel tasks.

Current mailbox scope:

* client-defined message type (`Mailbox<T>`)
* heap-preallocated fixed-capacity ring buffer
* non-blocking `try_send` / `try_recv`
* blocking `send` / `recv`
* preallocated bounded send and recv wait queues
* one bootstrap-created demo mailbox owned by the demo task module

Mailbox state is protected by the shared IRQ-save lock abstraction. In the
current no-SMP build that means local IRQ masking plus contention checks; the
same abstraction is the intended upgrade point for a future SMP spinlock.
Blocking waits enter the scheduler through a typed synchronous task-call path,
which lets the IPC layer recheck the wait condition and join waiter insertion
with scheduler blocking. This avoids heap allocation and lost wakeups in the
preemption-critical path.

## Build and run

```bash
just doctor
just build-aarch64
just run-aarch64
just debug-aarch64
just gdb-aarch64
```

With explicit log level:

```bash
just run-aarch64 debug
just run-aarch64 trace
```

Or via `xtask`:

```bash
cargo xtask run-aarch64 --log-level debug
cargo xtask run-aarch64 --log-level trace
```

## Immediate priorities

The best next steps are:

1. mailbox timeout integration on top of `kernel::time`
2. page-table allocation groundwork
3. growable heap design on top of frame allocation

## Documentation

* `docs/month1-plan.md` — month 1 closure and actual outcome
* `docs/month2-plan.md` — roadmap for the next month
* `ai-docs/decision-records/ADR-0001-architecture-strategy.md`
* `ai-docs/decision-records/ADR-0002-aarch64-irq-path-gicv2-timer.md`
* `ai-docs/decision-records/ADR-0003-aarch64-preemptive-irq-return-switching.md`
* `ai-docs/decision-records/ADR-0004-aarch64-boot-exception-separation-and-fatal-path.md`
* `ai-docs/decision-records/ADR-0005-one-shot-timer-deadline-engine.md`
* `ai-docs/decision-records/ADR-0006-time-owned-timed-events.md`
* `ai-docs/decision-records/ADR-0007-dtb-memory-map-and-frame-allocator.md`
* `ai-docs/decision-records/ADR-0008-aarch64-softfloat-kernel-target.md`
* `ai-docs/decision-records/ADR-0009-bootstrap-kernel-heap-on-frame-allocator.md`
* `ai-docs/decision-records/ADR-0010-irq-safe-kernel-heap-lock-and-allocation-policy.md`
* `ai-docs/decision-records/ADR-0011-dynamic-preallocated-scheduler-and-time-structures.md`
* `ai-docs/decision-records/ADR-0012-bounded-mailbox-ipc.md`
