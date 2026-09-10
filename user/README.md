# Freestanding userspace

genrt userspace consists of statically linked freestanding AArch64 C programs.
Programs share a small freestanding support library, not a full libc. There is
no dynamic linker, TLS runtime, ELF interpreter, or hosted process startup
environment.

## Product programs

`user/c/programs.toml` is the sole production program registry. Each entry
declares source, initramfs install path, contract role, and dynamic case. `xtask`
uses it for compilation, contract staging, release staging, identity checks, and
coverage reporting.

Current products are the shell and `/bin/echo`, `/bin/cat`, `/bin/ls`,
`/bin/pwd`, `/bin/taskset`, `/bin/sleep`, `/bin/reboot`, and `/bin/poweroff`.
Other example C files may be buildable without entering production composition.

`taskset <cpu> <program> [args...]` launches one new process on a decimal
logical CPU. A program containing `/` is passed to `execve` unchanged;
otherwise `taskset` uses `/bin/<program>`. Unlike Linux `taskset`, this utility
does not accept masks or modify an existing process: it applies the existing
one-shot fork placement policy, and the child's affinity becomes immutable at
publication.

To add a production binary:

1. add freestanding C source under `user/c/`;
2. register it in `user/c/programs.toml`;
3. add an exact invocation to `tests/qemu/program-contracts.toml`;
4. update the owning supervisor contract and run the targeted QEMU case;
5. run `cargo xtask ci` and release composition checks when applicable.

## Interactive shell

The shell keeps the last 16 submitted non-empty commands in a fixed ring.
Whitespace-only and consecutively identical lines are not added; overflow
discards the oldest entry. Up/Down (CSI or SS3 arrows) recall commands into an
independent editing buffer. Down past the newest entry restores the draft
saved by the first Up, including its line-overflow state. Editing a recalled
command never modifies the stored entry. History is process-private and is
not persisted; no history files are created.

Tab completes only a bare first command token, including after leading spaces.
Candidates are `cd`, `exit`, and regular ELF `ET_EXEC` entries under `/bin`.
The directory ABI has no mode field; completion checks ELF identification/type,
while `execve` still validates the complete image. Built-ins take precedence
over same-named files. Candidate names must be printable ASCII without spaces
or slashes and fit the shell's existing 128-byte execution path buffer.

A unique match is completed with a trailing space if it fits. Multiple matches
extend the longest common prefix; if it cannot grow, candidates are displayed
and the input is redrawn. No match leaves the line unchanged. Tab inside an
argument or a path does nothing. There is no PATH lookup, filename completion,
quoting, scripting, or arbitrary cursor movement.

Input remains byte-oriented with append/backspace editing, a 256-byte line
limit, and a 16-argument limit. An overlong submitted line is rejected rather
than executing its truncated prefix. ANSI cursor save/restore and erase are
used to redraw the input, including wrapped lines. Unknown CSI/SS3 sequences
are discarded through their final byte; unsupported OSC/DCS strings are
discarded through BEL or ST. Newline, backspace, and Tab resynchronize partial
sequences without copying control payloads into commands. CRLF submits once.

Editor/history state and directory scan storage are fixed-size; no shell heap
allocation is introduced. Completion streams the immutable `/bin` index, so
work is linear in its finite entries without a candidate-count truncation.
At most one directory fd and one candidate-file fd are open during a scan.
The shell-private [input helpers](c/shell_input.h) are shared only with host
tests, not exposed as support-library API. `shell-contract` exercises the exact
production shell using controlled test commands, not prompt assertions.

## Build and startup

The common linker script creates ELF64 little-endian AArch64 `ET_EXEC` images
with static `PT_LOAD` segments. `user/c/aarch64/crt0.S` reads the kernel-built
initial stack, calls `main(argc, argv)`, and exits through the syscall ABI.

The initial stack and syscall conventions are documented in
[`user/c/aarch64/README.md`](c/aarch64/README.md). Raw syscall transport and
numbers stay in `user/c/aarch64/include/syscall.h`. Public headers, including
`sched.h`, `time.h`, `unistd.h`, and `sys/reboot.h`, live in `user/c/include/`
and use the architecture adapter through the configured include paths.

Common [string/memory, formatting, and parsing code](c/common/README.md) is
statically linked into every production, example, and QEMU userspace ELF.
Products and tests use the same implementations; protocol emitters remain
test-only. See [ADR-0046](../memory/decisions/ADR-0046-shared-freestanding-userspace-library.md)
for the library and architecture boundary. Existing build commands require no
per-program library configuration.

## Initramfs

`user/initramfs/` contains product data files. `xtask` stages registered ELF
programs and this data into a deterministic readonly `newc` archive. `/init` is
the shell by default. The kernel parses the archive and loads ELF segments; QEMU
does not set the userspace PC.

## ABI boundaries

- Syscall number in `x8`, arguments in `x0..x5`, return in `x0`.
- Errors are negative errno values.
- Pathnames, argv/envp, I/O, and directory buffers are bounded.
- `fork` eagerly copies process memory; `execve` replaces it from ramfs.
- `<sched.h>` exposes fixed CPU sets and immutable process-affinity queries.
- `<sys/reboot.h>` exposes Linux-compatible restart and power-off operations;
  the wrapper hides raw reboot magic values.
- `<time.h>` and `<unistd.h>` expose monotonic clock observation, relative and
  absolute sleeps, and conventional `sleep`/`usleep` wrappers.
- Shell line editing and command policy are userspace responsibilities.

Do not add libc assumptions or raw flat-binary loading paths. Syscall changes
require an ADR, synchronized kernel/header updates, and exact contract coverage.
