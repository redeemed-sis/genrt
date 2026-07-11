# genrt

**genrt** is an experimental hard real-time operating system project written primarily in **Rust**.

Current active target:

* **AArch64**
* **Rust target `aarch64-unknown-none-softfloat`**
* **QEMU `virt`**
* **single-core EL1 kernel threads + first EL0 userspace bring-up**
* **high-half kernel with AArch64 stage-1 MMU enabled**
* **QEMU-first bring-up and debugging**

## Current status

The current AArch64 path already has:

* low-linked `.boot.*` trampoline that runs before MMU enable
* high-linked kernel sections loaded at low physical addresses
* AArch64 stage-1 MMU bring-up with temporary TTBR0 identity mappings
* TTBR1 high direct map using `KERNEL_HVA_OFFSET = 0xffff_0000_0000_0000`
* post-memory-init switch from boot-owned page tables to allocator-owned runtime TTBR1 tables
* low identity mapping removal after the high kernel is established
* `VBAR_EL1` exception-vector setup at high VA
* PL011 UART output through high virtual MMIO aliases
* `BootInfo` handoff into Rust
* DTB-seeded physical memory discovery
* QEMU `virt,gic-version=2` DTB loaded by `xtask` at `0x4000_0000`
* low `.boot.text` DTB parser for RAM, PL011, and GICv2 initial mappings
* QEMU `virt` emergency platform fallback for early diagnostic reachability
* internal physical memory map with reserved-range carving
* page-aligned usable frame ranges
* minimal free-list physical frame allocator
* architecture-agnostic generic frame allocator that continues to return physical frames
* fixed-size bootstrap kernel heap on `linked_list_allocator`
* heap initialized through a high virtual pointer over a physical frame range
* single-core IRQ-safe heap lock for task-context allocation/free
* working `alloc` container smoke tests (`Vec`, `VecDeque`, `BinaryHeap`, `BTreeMap`)
* GICv2 initialization through high virtual MMIO aliases
* PL011 RX interrupts routed through GICv2 for UART-backed stdin
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
* timeout-aware mailbox send/receive operations
* bounded `thread_spawn` / `thread_exit` / `thread_join`
* first EL0 userspace `/init` loaded from QEMU-provided initramfs as an ELF64 AArch64 executable
* minimal kernel ELF loader for static freestanding `ET_EXEC` images
* minimal TTBR0 user address space with 4 KiB user page mappings
* scheduler TTBR0 activation for user threads and TTBR0 clear for kernel threads
* lower-EL `svc #0` syscall dispatch separated from EL1 task-call `svc #0`
* POSIX-like `open` / `read` / `write` / `close` syscall path for the first user process
* POSIX-like `fork` / `execve` / `waitpid` process-control path for child processes
* blocking `read(0)` over UART stdin using a bounded kernel RX ring and scheduler wakeup
* readonly initramfs-backed ramfs and per-process bounded FD table
* interactive userspace shell demo that opens files and runs `/bin/echo` from initramfs
* bounded process table with generation-checked `ProcessId`
* process exit/fault status and kernel-side `process_join`
* lower-EL user fault policy that terminates the current process instead of panicking the kernel
* minimal allocation-free formatted logging with log levels
* improved fatal exception diagnostics
* `xtask` post-link `.boot.text` autonomy check using `readelf` and `llvm-objdump`

In one sentence:

> genrt is currently an early **single-core high-half preemptive kernel prototype with a first EL0 userspace smoke path** on AArch64/QEMU.

The AArch64 build currently uses the Rust target `aarch64-unknown-none-softfloat`.
This is intentional for the current kernel stage: the scheduler/trap path does not
yet own FP/SIMD state, so the build avoids implicit hard-float/AdvSIMD assumptions
in ordinary Rust code.

## What is not implemented yet

* SMP scheduling
* mailbox registry / dynamic mailbox creation
* driver model
* low-overhead buffered tracing
* demand paging / recoverable user page faults / full process model
* ASIDs and multiple TTBR0 userspace address spaces

## Execution model

High-level flow:

```text
_start (.boot.text, low physical/identity)
  -> park secondary CPUs
  -> set low boot stack
  -> boot_build_page_tables()
       -> parse QEMU-loaded DTB from platform boot slot
       -> build TTBR0 identity mappings
       -> build TTBR1 high direct-map/MMIO mappings
  -> program MAIR_EL1 / TCR_EL1 / TTBR0_EL1 / TTBR1_EL1
  -> enable SCTLR_EL1.M/C/I
  -> switch SP to high boot-stack alias
  -> branch to high rust_entry

rust_entry (high VA)
  -> zero high .bss
  -> set high VBAR_EL1
  -> initialize UART/GIC high MMIO aliases
  -> BootInfo + DTB memory discovery through HVA
  -> kernel_main()
  -> physical memory init
  -> switch to allocator-owned runtime kernel page tables
  -> clear TTBR0 temporary identity mappings
  -> bootstrap scheduler
  -> start first kernel task from prepared trap frame

kernel init thread
  -> create first TTBR0 user address space
  -> find /init in mounted initramfs
  -> parse /init as user ELF image
  -> map ELF PT_LOAD segments
  -> map user stack
  -> spawn EL0 user thread
  -> join user process and log exit/fault status

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

UART RX IRQ
  -> acknowledge GIC interrupt
  -> drain PL011 RX FIFO into bounded kernel stdin ring
  -> wake one stdin-blocked thread if present
  -> return through normal IRQ path

EL0 read(0)
  -> if stdin ring has bytes, copy bytes to userspace with copy_to_user()
  -> if stdin ring is empty, register the current thread as stdin waiter
  -> rewind ELR_EL1 by one AArch64 SVC instruction
  -> block on BlockReason::StdinRead and run another task or idle
  -> after UART wake, restart the same read(0) syscall and return bytes

EL0 shell external command
  -> fork()
     parent: waitpid(child, &status, 0)
     child:  execve("/bin/<command>", argv, NULL)
  -> execve loads the ELF from initramfs, replaces the child's TTBR0 image,
     builds argc/argv on the new user stack, and returns through eret to _start
```

Key milestone already reached:

> **task switching is performed by replacing the IRQ return frame, not by a normal function-call-style switch**

## Current limitations

* single-core only
* process control is intentionally minimal: one main user thread per process
* `fork` uses eager address-space copying; no copy-on-write yet
* `waitpid` supports a specific child pid with options `0`; no `waitpid(-1)` yet
* `execve` supports bounded `argv` and `envp` strings on the initial user stack
* shell command lookup is limited to `/bin/<command>` for names without `/`
* no ASIDs or multiple per-process TTBR0 roots yet
* VM API currently supports only 2 MiB-aligned TTBR1 kernel mappings
* user VM bring-up supports only explicit 4 KiB mappings created by the first process path
* userspace ELF file size is bounded by a fixed bring-up loader reservation
* heap is currently a fixed-size `16 MiB` bootstrap region
* direct-to-UART logging
* scheduler/time dynamic containers are preallocated at bootstrap and must not grow in IRQ paths
* heap does not grow from arbitrary frames yet
* no SMP TLB shootdown
* platform-specific boot protocol and MMIO discovery live in the AArch64 platform layer

## Repository layout

```text
genrt/
├── arch/aarch64/      # AArch64 boot, MMU, traps, timer, GIC, platform discovery
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

## AArch64 MMU And Boot Protocol

The current AArch64 strategy is:

```text
low-linked trampoline + high-linked kernel loaded low
```

`.boot.*` sections have low VMA/LMA and execute before the MMU is enabled. The
main kernel sections are linked at high virtual addresses but loaded at low
physical addresses via linker `AT(...)`; no segment copy is performed.

Address convention:

```text
KERNEL_HVA_OFFSET = 0xffff_0000_0000_0000
HVA = PA + KERNEL_HVA_OFFSET
PA  = HVA - KERNEL_HVA_OFFSET
```

The bootstrap page tables are intentionally small:

* TTBR0 temporarily maps the low identity window needed by the trampoline and DTB access.
* TTBR1 maps the high direct-map RAM window and high Device mappings for UART/GIC.
* After `kernel::memory::init()`, the kernel switches to allocator-owned TTBR1 tables and clears TTBR0.

`xtask` controls the QEMU bare-metal protocol. It generates a compact QEMU
`virt,gic-version=2` DTB and loads it at `0x4000_0000` with a loader device. The
kernel image stays at `0x4008_0000`. It also builds freestanding AArch64 user
ELF examples and loads the selected ELF file as raw bytes at the reserved
bring-up physical address `0x4700_0000` with
`-device loader,...,force-raw=on`; the loader does not change the CPU PC. The
kernel parses the ELF image itself and maps `PT_LOAD` segments into TTBR0.
The low `.boot.text` parser reads the DTB before UART/GIC are initialized and
extracts only the ranges needed for initial MMU mappings. If that early parse
fails, the AArch64 QEMU platform layer has an emergency fallback for RAM/UART/GIC
so early diagnostics can still reach UART.

The generic frame allocator remains MMU-agnostic: it manages physical frames and
returns `PhysAddr`. PA-to-HVA conversion happens only at explicit dereference
boundaries such as DTB reads, free-list metadata inside frames, heap init,
page-table writes, and MMIO access.

The build command runs a post-link `.boot.text` autonomy check. It verifies that
the pre-MMU boot code has no relocations, no runtime helper thunks such as
`memcpy`/`memset`/panic/formatting, no high-VA instruction operands, and no
direct branch/call out of `.boot.*`.

## Heap

The kernel heap is currently initialized from one contiguous `16 MiB` region
allocated out of the physical frame allocator during early memory bootstrap.

Initialization order is:

1. parse and normalize physical memory regions
2. initialize the frame allocator on usable page ranges
3. allocate one contiguous heap range via `alloc_contiguous`
4. convert the physical heap range to HVA at the heap boundary
5. initialize `linked_list_allocator`
6. run heap-backed smoke tests

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

## Virtual Memory API

The first VM API is deliberately narrow and kernel-only. It supports TTBR1
kernel mappings after runtime page tables are active:

* `phys_to_virt`
* `virt_to_phys_direct`
* `translate_kernel_va`
* `map_kernel_region`
* `unmap_kernel_region`
* `protect_kernel_region`
* `drop_boot_identity_mapping`
* `switch_to_runtime_kernel_tables`

Mutation APIs return `VmError::NotInitialized` until
`switch_to_runtime_kernel_tables()` has replaced boot-owned tables from
`.boot.bss` with frame-allocator-owned page tables. This prevents mappings from
being added to tables that will be discarded and prevents reclaiming boot table
storage through the generic frame allocator.

The first userspace path adds a separate narrow TTBR0 API:

* create/destroy one user address space root from physical frames
* map 4 KiB EL0 pages for ELF segments and user stack
* translate user VA for bring-up `copy_from_user` validation
* activate a user TTBR0 root or clear TTBR0 during scheduler handoff

The current userspace loader accepts only ELF64 little-endian AArch64
`ET_EXEC` files with static `PT_LOAD` segments. It rejects dynamic linking,
interpreters, TLS, non-AArch64 images, and RWX segments. User examples are built
from `user/c/` with a tiny freestanding C runtime and linker script. QEMU loads
`initramfs.cpio`, not a direct user ELF payload; the kernel mounts that archive
and loads `/init` through the ELF loader.

The initial user syscall ABI is AArch64-style: `x8` is the syscall number,
`x0..x5` are arguments, and `x0` is the return value. The current POSIX-like
subset includes `open`, `read`, `write`, `close`, and `exit`; errors are
reported as negative errno values.

The first readonly filesystem is an initramfs-backed ramfs with exact path
lookup. `xtask` builds a deterministic uncompressed `newc` cpio archive from
`user/initramfs/` plus `/init`, currently `shell.elf`. The shell can open
`/hello.txt`, `/etc/banner`, and `/readme.txt`; pathname scanning is bounded by
`GENRT_PATH_MAX = 4096` bytes.

`read(0)` is backed by PL011 RX interrupts rather than polling. The kernel keeps
only raw bytes in a bounded stdin ring and does not implement terminal line
discipline. The `shell.elf` demo owns echo, backspace, Enter handling, and turns
each entered line into a pathname for `open()`.

## IPC

The first IPC primitive is a bounded mailbox for EL1 kernel tasks.

Current mailbox scope:

* client-defined message type (`Mailbox<T>`)
* heap-preallocated fixed-capacity ring buffer
* non-blocking `try_send` / `try_recv`
* blocking `send` / `recv`
* timeout-aware `send_until_counter` / `recv_until_counter`
* explicit duration wrappers in ticks, microseconds, and milliseconds
* preallocated bounded send and recv wait queues
* one bootstrap-created demo mailbox owned by the demo task module

Mailbox state is protected by the shared IRQ-save lock abstraction. In the
current no-SMP build that means local IRQ masking plus contention checks; the
same abstraction is the intended upgrade point for a future SMP spinlock.
Blocking waits enter the scheduler through a typed synchronous task-call path,
which lets the IPC layer recheck the wait condition and join waiter insertion
with scheduler blocking. This avoids heap allocation and lost wakeups in the
preemption-critical path.

IPC timeouts are represented as typed time events rather than callbacks. The
scheduler stores an opaque IPC wait token and timeout event; normal IPC wakeup
cancels the event, while timeout dispatch asks IPC to remove the task from the
owning wait queue before waking it with a timeout result.

## Kernel threads

Kernel tasks now have a bounded thread lifecycle API:

* `kernel::sched::thread_spawn(entry, ThreadArg, attrs)`
* `kernel::sched::thread_exit(code)`
* `kernel::sched::thread_join(id)`

Thread handles are `ThreadId { index, generation }` values. The index names a
preallocated scheduler slot; the generation changes before a freed slot is
reused, so stale handles fail validation instead of naming a later thread.

Thread slots, stacks, saved frames, and ready queue capacity are prepared during
scheduler bootstrap. Runtime spawn does not grow scheduler containers; it
initializes a free slot, prepares its trap frame, and queues it. Returning from a
spawned thread entry goes through the same controlled SVC path as explicit
`thread_exit`, which records the exit code, wakes a single joiner if present,
and never resumes the exited thread. Successful join reclaims the slot for reuse.
Bootstrap/static tasks use the same `fn(ThreadArg) -> usize` entry shape as
runtime-spawned threads; `ThreadArg` can carry a small integer or an explicit
raw pointer when a caller needs richer Rust-owned context.

The current stack class is fixed at 8 KiB per thread slot. Detached threads are
supported by `ThreadAttrs::detached()` and are reclaimed on exit; the demo uses
joinable workers to exercise `spawn -> exit -> join`.

## Build and run

```bash
just doctor
just build-aarch64
just build-user-hello
just build-user-fault
just build-user-read-file
just build-user-shell
just build-initramfs
just run-aarch64
just run-aarch64-read-file
just run-aarch64-shell
just run-aarch64-fault
just debug-aarch64
just debug-aarch64-shell
just debug-aarch64-fault
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
cargo xtask build-initramfs --root user/initramfs --output target/aarch64-unknown-none-softfloat/debug/initramfs.cpio
```

Interactive shell:

```bash
just run-aarch64-shell
```

The shell accepts paths such as `/hello.txt`, `/etc/banner`, and `/readme.txt`;
`exit` terminates the userspace process. The shell recipes default to `info`
logs so UART input remains readable. Default QEMU runs load
`target/aarch64-unknown-none-softfloat/debug/initramfs.cpio` at the reserved
initramfs physical window.

QEMU is run with `-serial mon:stdio`, so stdin goes to the emulated UART while
QEMU monitor escape commands are still available:

* `Ctrl-a x` exits QEMU.
* `Ctrl-a c` switches between serial and monitor.

## Immediate priorities

The best next steps are:

1. refine VM permissions and page-table ownership invariants
2. add richer initramfs/VFS lookup semantics such as readdir/stat
3. fault-aware `copy_from_user` recovery for faults during actual loads/stores
4. evolve UART stdin into a real TTY/console subsystem without changing the fd ABI
5. growable heap design on top of frame allocation

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
* `ai-docs/decision-records/ADR-0013-mailbox-timeout-semantics.md`
* `ai-docs/decision-records/ADR-0014-bounded-kernel-thread-lifecycle.md`
* `ai-docs/decision-records/ADR-0015-aarch64-high-half-mmu-bring-up.md`
* `ai-docs/decision-records/ADR-0016-first-aarch64-el0-process.md`
* `ai-docs/decision-records/ADR-0017-process-table-and-user-fault-policy.md`
* `ai-docs/decision-records/ADR-0018-userspace-elf-loader.md`
* `ai-docs/decision-records/ADR-0019-readonly-ramfs-and-fd-table.md`
* `ai-docs/decision-records/ADR-0020-uart-stdin-and-shell.md`
* `ai-docs/decision-records/ADR-0021-initramfs-cpio-root.md`
