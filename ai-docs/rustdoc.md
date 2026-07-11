# Rustdoc Policy

Rust API documentation is part of the kernel contract. It is required for every
new or changed `pub` and `pub(crate)` item that can be called from another
module.

## Required Function Sections

Every documented function must cover:

- what the function does and which invariant it relies on;
- `# Arguments` for every parameter, using the parameter's exact name;
- `# Returns` for every successful return shape, including `Option::None` and
  EOF-style cases;
- `# Errors` for `Result` APIs, including the important error variants and what
  conditions produce them;
- `# Safety` for unsafe functions or functions whose safety depends on caller
  obligations;
- `# Panics` if the function can panic by design.

## Determinism Notes

If an API allocates, blocks, enters a scheduler/task-call path, touches IRQ
state, or mutates global process/filesystem state, document that behavior in the
summary or relevant section. This keeps hard real-time invariants visible at the
call site.

## Scope

Private helper functions may use normal code comments when their contract is
local and obvious. If a private helper encodes a subtle invariant that future
callers are likely to reuse, prefer rustdoc even before widening visibility.
