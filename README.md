# genrt

**genrt** is an experimental hard real-time operating system project written primarily in **Rust**.

Current active target:

* **AArch64**
* **QEMU `virt`**
* **single-core EL1 kernel threads**
* **QEMU-first bring-up and debugging**

## Current status

The current AArch64 path already has:

* boot entry in `boot.S`
* `VBAR_EL1` exception-vector setup
* early PL011 UART output
* `BootInfo` handoff into Rust
* GICv2 initialization
* periodic architected timer interrupts
* kernel tick accounting
* full trap-frame save/restore on IRQ
* **IRQ-return-based preemptive task switching**
* static per-task stacks
* round-robin scheduling for runnable kernel tasks
* scheduler ownership isolated to kernel bootstrap and timer IRQ paths
* timer-driven sleep/wakeup with blocked tasks and automatic wake on tick
* minimal allocation-free formatted logging with log levels
* improved fatal exception diagnostics

In one sentence:

> genrt is currently an early **single-core preemptive EL1 kernel prototype** on AArch64/QEMU.

## What is not implemented yet

* MMU / virtual memory
* EL0 / user mode
* SMP scheduling
* bounded IPC/mailboxes
* driver model
* low-overhead buffered tracing

## Execution model

High-level flow:

```text
_start (boot.S)
  -> early arch init
  -> GICv2 init
  -> periodic timer init
  -> kernel_main()
  -> start first task from prepared trap frame

Timer IRQ
  -> save full TrapFrame
  -> identify timer interrupt
  -> rearm timer
  -> kernel::on_tick_interrupt(frame)
    -> update ticks
    -> wake sleeping blocked tasks with an O(N) scan
    -> scheduler selects next task
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
* direct-to-UART logging
* sleep/wakeup uses a simple O(N) task-table scan per tick
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

1. bounded mailbox/queue IPC
2. lightweight trace buffering
3. only then MMU and isolation work

## Documentation

* `docs/month1-plan.md` — month 1 closure and actual outcome
* `docs/month2-plan.md` — roadmap for the next month
* `ai-docs/decision-records/ADR-0001-architecture-strategy.md`
* `ai-docs/decision-records/ADR-0002-aarch64-irq-path-gicv2-timer.md`
* `ai-docs/decision-records/ADR-0003-aarch64-preemptive-irq-return-switching.md`
