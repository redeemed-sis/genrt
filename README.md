# genrt

**genrt** is an experimental hard real-time operating system project for modern CPU architectures, with **Rust as the primary implementation language** and a **QEMU-first bring-up workflow**.

The current focus is:
- **AArch64 on QEMU `virt`**
- early kernel bring-up and execution model design
- deterministic interrupt, timer, and scheduler foundations
- an AI-agent-friendly engineering workflow

Longer term, the project is intended to explore support for:
- **AArch64**
- **x86_64**
- **RISC-V**

---

## Current status

The repository is no longer just a scaffold. On the current AArch64/QEMU path, genrt already has:

- EL1 boot entry on QEMU `virt`
- AArch64 exception vectors (`VBAR_EL1`)
- `.bss` clearing and Rust handoff via `rust_entry`
- early UART output through **PL011**
- a minimal `BootInfo` contract with `dtb_pa`
- **GICv2** initialization
- EL1 physical timer programming through the architected timer registers
- working IRQ delivery and acknowledgment path
- periodic system ticks
- kernel time accounting
- a fixed-priority **scheduler skeleton** with an idle task and a test task

What is **not** implemented yet:
- full context switching between independent task stacks
- sleep/wakeup queues
- IPC primitives
- MMU-based virtual memory
- user mode / EL0 support
- SMP scheduling
- a stable driver model

In other words, genrt has moved past “first boot” and “first interrupt” and is now shaping its early **execution model**.

---

## Design goals

genrt is being built around a few early principles:

- **Determinism first**
  Keep early interrupt and scheduling paths bounded and easy to reason about.

- **QEMU-first bring-up**
  Stabilize architecture, debug loops, and execution flow in emulation before broadening platform support.

- **Rust-first kernel core**
  Keep most kernel logic in Rust and isolate architecture-specific `unsafe` code as much as possible.

- **Small, explicit milestones**
  Prefer clear bring-up checkpoints over premature subsystem expansion.

- **Agent-friendly workflow**
  The repo is structured to work well with AI coding agents using `AGENTS.md`, ADRs, helper docs, and `xtask`/`just` commands.

---

## Repository layout

```text
genrt/
├── AGENTS.md
├── Cargo.toml
├── justfile
├── rust-toolchain.toml
├── kernel/                  # architecture-neutral kernel code
├── arch/
│   ├── aarch64/             # current active bring-up target
│   ├── x86_64/              # placeholder
│   └── riscv64/             # placeholder
├── platform/                # platform-specific code (to grow over time)
├── crates/
│   └── bootinfo/            # early boot handoff structures
├── drivers/                 # future driver framework/bus/class split
├── tools/
│   └── xtask/               # workflow helper commands
├── tests/
├── examples/
├── docs/
└── ai-docs/
    ├── architecture.md
    ├── commits.md
    ├── debugging.md
    └── decision-records/
```

---

## Implemented kernel path on AArch64/QEMU

At a high level, the current execution path looks like this:

```text
_start (boot.s)
  -> set up EL1 boot environment
  -> clear .bss
  -> install VBAR_EL1
  -> call rust_entry(dtb_pa)
    -> early console output
    -> initialize GICv2
    -> initialize architected timer
    -> enter kernel_main(&BootInfo)
      -> initialize scheduler skeleton
      -> spin in kernel loop

Timer IRQ
  -> arch/aarch64 timer handler
  -> re-arm next tick
  -> kernel::on_tick_interrupt()
    -> kernel::time::on_tick_interrupt()
    -> scheduler.on_tick()
```

This means the system already has:
- a working hardware interrupt path
- a periodic kernel heartbeat
- deterministic task selection logic

What it still lacks is the next major step: **real context switching**.

---

## Scheduler skeleton

The current scheduler is intentionally minimal.

Implemented behavior:
- static task table
- fixed-priority selection
- deterministic tie-breaker by lowest task ID
- idle task is always eligible
- one test task can be added to the ready set
- scheduler runs on every tick

Not implemented yet:
- separate task stacks
- saved CPU contexts
- switching from one running task to another
- blocking primitives or wakeup queues

This is a **policy skeleton**, not yet a full task execution subsystem.

---

## Requirements

The current development workflow expects a Linux host with at least:

- `cargo`
- `rustup`
- `just`
- `ld.lld`
- `qemu-system-aarch64`
- `gdb` or `aarch64-linux-gnu-gdb`

Recommended Rust targets:
- `aarch64-unknown-none`
- `x86_64-unknown-none`
- `riscv64gc-unknown-none-elf`

There is a helper command to validate the local toolchain:

```bash
just doctor
```

---

## Quick start

### 1. Check tools

```bash
just doctor
```

### 2. Build the current AArch64 target

```bash
just build-aarch64
```

This produces:

```text
target/aarch64-unknown-none/debug/genrt-aarch64.elf
```

### 3. Run on QEMU

```bash
just run-aarch64
```

### 4. Run under GDB wait mode

```bash
just debug-aarch64
```

In another terminal:

```bash
just gdb-aarch64
```

---

## Useful commands

```bash
just doctor
just phase0-check
just tree
just build-aarch64
just run-aarch64
just debug-aarch64
just gdb-aarch64
```

You can also inspect the generated QEMU/GDB commands directly:

```bash
cargo xtask qemu-cmd --arch aarch64
cargo xtask gdb-cmd --arch aarch64
```

---

## Logging

genrt now has a minimal allocation-free kernel logging path built on top of the
raw console backend.

Available macros:
- `kprint!` / `kprintln!` for plain formatted output
- `error!`, `warn!`, `info!`, `debug!`, `trace!` for level-tagged logging

Available levels:
- `Error`
- `Warn`
- `Info`
- `Debug`
- `Trace`

The active threshold is compile-time controlled:
- default builds use `Info`

You can override the threshold explicitly with Cargo features on the `kernel`
crate:
- `log-level-error`
- `log-level-warn`
- `log-level-info`
- `log-level-debug`
- `log-level-trace`

Example:

```bash
cargo build -p kernel --target aarch64-unknown-none --features log-level-debug
```

For normal repo workflow, `just` can pass log levels through `xtask`:

```bash
just build-aarch64 trace
just run-aarch64 trace
just debug-aarch64
just debug-aarch64 trace
```

`debug-aarch64` defaults to `debug` logging when no explicit level is passed.

The raw console path (`console::putc` / `console::puts`) still exists for panic
fallbacks, very early boot, and emergency diagnostics.

Trace logging is allocation-free and simple enough for occasional use in IRQ and
scheduler paths during bring-up, but high-volume trace output will perturb
timing and should remain debug-oriented.

## Expected bring-up output

The exact output evolves with the codebase, but current bring-up output typically includes messages such as:

```text
[INFO ] kernel_main entered
[INFO ] bootinfo: arch=aarch64
[INFO ] bootinfo: dtb=present
[INFO ] sched: irq-return preemptive switching initialized
[DEBUG] tick=100
[TRACE] sched: prev=0 next=1
```

Debug and trace messages are intended for bring-up observability and may change
as the kernel grows more structured tracing support.

---

## Architecture notes

The repository currently has a healthy split between:
- `kernel/` for architecture-neutral logic
- `arch/aarch64/` for CPU- and exception-specific code

A fuller `platform/` split is still expected as the AArch64 QEMU `virt` path grows. Right now, some machine-specific details still live close to the active AArch64 bring-up code because the project is in an early milestone-driven phase.

Important current implementation points:
- interrupt delivery uses **GICv2**
- the timer path uses the AArch64 architected timer
- timer re-arm is explicit on each tick
- scheduling policy is kernel-side, while hardware interaction remains in the architecture layer

---

## Engineering workflow

genrt is developed with an explicit engineering workflow in mind:

- `AGENTS.md` describes how AI agents should work in the repository
- `ai-docs/decision-records/` stores architecture decisions
- `ai-docs/commits.md` documents commit message conventions
- `tools/xtask` provides repeatable workflow commands
- `justfile` provides a simple operator-friendly command surface

Relevant documents:
- `AGENTS.md`
- `ai-docs/architecture.md`
- `ai-docs/debugging.md`
- `ai-docs/decision-records/ADR-0001-architecture-strategy.md`
- `ai-docs/decision-records/ADR-0002-aarch64-irq-path-gicv2-timer.md`

---

## Near-term roadmap

The next logical milestone is:

### First real context switch

Planned work:
- task contexts for AArch64
- per-task stacks
- initial task frame setup
- architecture-specific context switch routine
- scheduler decision integrated with actual execution switch

After that, likely next steps are:
- sleep/wakeup based on ticks
- bounded IPC primitives
- stronger timing and tracing support
- memory-management groundwork

---

## Project maturity

This is an **early-stage systems project**.

genrt is currently best understood as:
- a serious bring-up and architecture exploration effort
- a growing RTOS kernel prototype
- a foundation for future multi-architecture hard real-time experiments

It is **not** production-ready.

---

## License

Licensed under the **MIT** license.
