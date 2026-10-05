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
`/bin/pwd`, `/bin/taskset`, `/bin/sleep`, `/bin/rtbench`, `/bin/reboot`, and
`/bin/poweroff`.
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

## Periodic timing benchmark

`rtbench -p period_ns -n steps -o operations` measures periodic userspace work
through the existing `CLOCK_MONOTONIC` API. All three options are required,
exactly once. Period is a nonzero decimal u64; steps are decimal 1..10000;
operations are decimal 0..4294967295. Signs, whitespace, suffixes, overflow,
unknown options, and extra operands are rejected before observing the clock.
The shared `getopt` parser accepts attached or separate short-option arguments.

The initial observation defines `R[0]`. Each sample observes start `S`, executes
the payload, and observes end `E`. Releases advance only by
`R[i+1] = R[i] + period`, including after overruns; no periods are skipped and
the origin is never reset. Release latency is `S - R[i]`; elapsed task execution
time (TET) is `E - S`, including preemption and clock-call overhead, not CPU
accounting time. An overrun occurs strictly when `E > R[i+1]`, with duration
`E - R[i+1]`. Equality is not an overrun. Every non-overrun iteration sleeps
using `TIMER_ABSTIME` until `R[i+1]`, including the final iteration.

There are exactly `steps` samples, at releases `R[0]` through `R[steps-1]`.
The final sleep targets `R[steps]`; it is not an additional sample. Both
`period * steps` and the initial absolute horizon `R[0] + period * steps` are
checked before payload execution. Timespec normalization, conversion,
monotonic ordering, and release arithmetic are checked as well.

One static `.bss` array holds 10000 samples of three u64 fields (240000 bytes).
There is no heap allocation, large stack array, or normal output during the
measurement loop or its final sleep. Each step executes exactly `operations`
unsigned integer additions, with an empty register compiler barrier after
each add to prevent elimination. Loop control is not payload work. Zero is
accepted; there are no payload multipliers, floating-point, or SIMD operations.
The accumulator starts at zero, carries across steps, and is consumed in the
report as `result = steps * operations`.

After measurement, checked sums are accumulated, then every zero-based `STEP`
row reports `release_latency_ns`, `tet_ns`, and `overrun_ns`. The final
`SUMMARY` reports configuration, `release_latency_avg_ns`,
`release_latency_max_ns`, `tet_avg_ns`, `tet_max_ns`, `overrun_avg_ns`,
`overrun_max_ns`, `overruns`, and `result`. Averages are integer-truncated
over all requested samples, including zero overruns. Output is diagnostic,
not a machine test protocol. Exit status is 0 for success, 2 for CLI errors,
and 1 for runtime, timestamp, arithmetic, statistics, or output failure
(including failure to print a CLI diagnostic). Failed runs need not emit a
complete report; an output failure can leave a printed prefix.

For a production shell session:

```text
rtbench -p 1000000 -n 100 -o 50000
taskset 1 rtbench -p 1000000 -n 100 -o 50000
```

The second command requires logical CPU1 online and uses `taskset`'s immutable
child placement. Period is 1 ms; workload counts are not a duration guarantee.
QEMU contracts establish functionality, not hardware latency or a worst-case
execution bound. See the [validation procedure](../docs/testing.md#rtbench-validation)
for host, virtual-board, and user-performed Raspberry Pi checks.

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
  Public `clock_gettime`/`clock_getres` read the physical counter at EL0 without
  SVC. Non-NULL outputs must be writable C objects; raw clock syscalls retain
  kernel pointer validation and EFAULT. Sleep still enters the kernel.
- Shell line editing and command policy are userspace responsibilities.

Do not add libc assumptions or raw flat-binary loading paths. Syscall changes
require an ADR, synchronized kernel/header updates, and exact contract coverage.
