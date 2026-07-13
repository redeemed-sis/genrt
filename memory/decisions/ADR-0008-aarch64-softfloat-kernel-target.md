# ADR-0008: AArch64 Kernel Build Uses `aarch64-unknown-none-softfloat`

## Status

Accepted

## Context
`genrt` currently runs as an EL1 kernel on AArch64/QEMU and already has:
- exception vectors,
- full trap-frame save/restore,
- IRQ-return-based preemption,
- scheduler and timed-event dispatch.

At this stage the kernel does **not** yet own FP/SIMD state:
- no AdvSIMD/FPU enable policy,
- no trap handling for first use,
- no save/restore in the scheduler context path.

During DTB parser investigation, high-level `fdt-raw` traversal reproducibly
faulted under `gdb` with:
- `ESR_EL1.EC = 0x07` (FP/AdvSIMD trap),
- `ELR_EL1` in `core::slice::raw::from_raw_parts::precondition_check`,
- a faulting `cnt v0.8b, v0.8b` instruction.

That showed the failure was not corrupted DTB memory, but ordinary Rust code
reaching AdvSIMD instructions through the hard-float AArch64 target/codegen
assumptions.

## Decision
- Switch the AArch64 kernel build target from `aarch64-unknown-none` to
  `aarch64-unknown-none-softfloat`.
- Update build, run, debug, and helper scripts to use the soft-float target
  consistently.
- Keep the kernel in this target mode until it explicitly owns FP/SIMD policy
  and context management.

## Rationale
This is the smallest correct fix for the current kernel stage:
- it aligns the Rust target ABI/codegen with current kernel invariants,
- avoids dragging FP/SIMD support into trap and scheduler code prematurely,
- removes a whole class of early-boot failures caused by implicit use of FP or
  AdvSIMD in ordinary dependency code.

It is also cleaner than piecemeal `target-feature` workarounds, because the
target itself now expresses the kernel's real execution contract.

## Consequences
- The reproducible AArch64 workflow now builds under
  `aarch64-unknown-none-softfloat`.
- Output paths move under `target/aarch64-unknown-none-softfloat/...`.
- External crates such as DTB parsers can use more of their normal code paths
  without forcing immediate FP/SIMD support in the kernel.
- If the kernel later decides to allow FP/SIMD in EL1 code, that should be a
  deliberate follow-up design step with explicit trap/save/restore policy.
