// Early AArch64 trampoline for the following strategy:
//   low-linked trampoline + high-linked kernel loaded low.
// Raspberry Pi continues from firmware EL2/EL1 into the QEMU high-half runtime.
//
// Requirements before enabling the MMU:
// - PC and SP must contain low physical/identity addresses.
// - High-linked symbols must not be dereferenced as pointers.
// - The main .bss must not be cleared through __bss_start/__bss_end because
//   those symbols already contain high virtual addresses.
// - VBAR_EL1 must not reference high __vectors before the TTBR1 mapping exists.
//
// Register contract for this file:
// - QEMU: x0 on entry is not part of the bare-metal boot protocol.
// - Raspberry Pi: preserve U-Boot's x0 DTB pointer before stack setup.
// - x19: architecture-only bootstrap stack slot (not a logical CpuId).
// - x20: low physical pointer to BOOT_MMU_PARAMS in .boot.bss.
// - x1..x6 after boot_build_page_tables(): MAIR/TCR/TTBR/SP/entry parameters.
//
// boot_build_page_tables() resides in .boot.text and populates only low
// .boot.bss page tables. After SCTLR_EL1.M=1, the current instruction still
// executes through the TTBR0 identity mapping; the explicit blr x6 transfers
// PC to the high-half Rust entry.
//
// There are two entry paths into the shared MMU programming tail:
//
//   _start
//     CPU0 entry selected by QEMU's bare-metal boot protocol. It builds all
//     bootstrap page tables and enters the global Rust initialization path.
//
//   secondary_start
//     Physical entry supplied to PSCI CPU_ON. It reuses immutable boot TTBR0
//     only for the low-to-high transition, installs CPU0-published runtime
//     TTBR1, and enters the secondary-only Rust registration path.
//
// Both paths prepare the same register contract before boot_program_mmu:
//   x1 = temporary identity TTBR0 root
//   x2 = high-half TTBR1 root
//   x3 = TCR_EL1
//   x4 = MAIR_EL1
//   x5 = high virtual stack top
//   x6 = high virtual Rust entry address
// On the runtime path x19 remains an architecture bootstrap-stack slot. It
// becomes a generic logical CpuId only after Rust validates hardware identity
// and registers the executing CPU.
.section .boot.text.entry, "ax"
.global _start
.type _start, %function

// Initial EL1 control before the boot page tables are installed. M (MMU) and
// C (data/unified cache) stay clear; I enables the instruction cache.
.equ PI_SCTLR_SA0,     (1 << 4)   // Check SP_EL0 alignment.
.equ PI_SCTLR_ITD,     (1 << 7)   // Restrict AArch32 IT instructions.
.equ PI_SCTLR_I,       (1 << 12)  // Enable the instruction cache.
.equ PI_SCTLR_NTWI,    (1 << 16)  // Do not trap EL0 WFI to EL1.
.equ PI_SCTLR_NTWE,    (1 << 18)  // Do not trap EL0 WFE to EL1.
.equ PI_SCTLR_RES1,    ((1 << 22) | (1 << 23) | (1 << 28) | (1 << 29))
// Bits 6 and 13 are RES0 on Cortex-A53. Retain the old write value here;
// removing them belongs in a separate boot-control change.
.equ PI_SCTLR_LEGACY_RES0, ((1 << 6) | (1 << 13))
.equ PI_SCTLR_BOOT, (PI_SCTLR_SA0 | PI_SCTLR_ITD | PI_SCTLR_I | PI_SCTLR_NTWI | PI_SCTLR_NTWE | PI_SCTLR_RES1 | PI_SCTLR_LEGACY_RES0)

// SPSR_EL2 is copied into PSTATE by ERET. Return to AArch64 EL1h (SP_EL1)
// with debug, SError, IRQ, and FIQ exceptions masked during early setup.
.equ PI_SPSR_EL1H, 0b0101       // M[3:0]: EL1h, not EL1t or AArch32.
.equ PI_SPSR_D,    (1 << 9)     // Mask debug exceptions.
.equ PI_SPSR_A,    (1 << 8)     // Mask asynchronous SError exceptions.
.equ PI_SPSR_I,    (1 << 7)     // Mask IRQs.
.equ PI_SPSR_F,    (1 << 6)     // Mask FIQs.
.equ PI_SPSR_BOOT, (PI_SPSR_EL1H | PI_SPSR_D | PI_SPSR_A | PI_SPSR_I | PI_SPSR_F)

// Firmware may release both primary and spin-table CPUs at EL2. No stack is
// needed here. x9 is scratch; arguments and the bootstrap slot remain intact.
.macro pi_enter_el1
    mrs x9, CurrentEL
    cmp x9, #4
    b.eq .Lpi_el1\@
    cmp x9, #8
    b.ne boot_terminal_park
    mov x9, #(1 << 31)    // HCR.RW: AArch64 EL1, no stage 2 or IRQ routing.
    msr HCR_EL2, x9
    msr CPTR_EL2, xzr
    mov x9, #3           // EL1 physical counter and timer access.
    msr CNTHCTL_EL2, x9
    msr CNTVOFF_EL2, xzr
    movz x9, #(PI_SCTLR_BOOT & 0xffff)
    movk x9, #((PI_SCTLR_BOOT >> 16) & 0xffff), lsl #16
    msr SCTLR_EL1, x9
    mov x9, #PI_SPSR_BOOT
    msr SPSR_EL2, x9
    adr x9, .Lpi_el1\@
    msr ELR_EL2, x9
    isb
    eret
.Lpi_el1\@:
    msr SPSel, #1
.endm

_start:
.if GENRT_PI_RUNTIME
    // Standard U-Boot checks this inner ARM64 Image header even in a FIT.
    // The shared linker accounts for all code, data, BSS, and boot stacks.
    b .Lfirmware_entry
    .long 0
    .quad 0                  // Zero text offset; base is 2 MiB aligned.
    .quad __arm64_image_size
    .quad 8                  // LE, unspecified page size, arbitrary placement.
    .quad 0, 0, 0
    .long 0x644d5241          // "ARM\x64".
    .long 0
.Lfirmware_entry:
    // Check incoming execution state before touching RAM or EL1 registers.
    mov x21, x0
    mrs x25, CurrentEL
    msr daifset, #0xf
    cmp x25, #4
    b.eq .Lboot_pi_el1
    cmp x25, #8
    b.ne boot_terminal_park
    mrs x27, SCTLR_EL2
    b .Lboot_pi_translation
.Lboot_pi_el1:
    mrs x27, SCTLR_EL1
.Lboot_pi_translation:
    // U-Boot hands off one boot CPU; no secondary may share slot zero.
    mrs x9, MPIDR_EL1
    and x9, x9, #0xff
    cbnz x9, boot_terminal_park
    tst x27, #1
    b.ne boot_terminal_park
    tst x27, #4
    b.ne boot_terminal_park
    // U-Boot may enter at EL2. Install a minimal non-virtualized EL1 regime:
    // AArch64 EL1, physical counter/timer access, no stage-2 translation or
    // interrupt routing to EL2. No PSCI hypervisor is invented by this path.
    pi_enter_el1
.endif
    // This is the boot CPU path on either platform. QEMU secondary CPUs use
    // `secondary_start` through PSCI CPU_ON with their intended slot in x0.
    // Keep asynchronous exceptions masked while high Rust installs VBAR and
    // initializes CPU0-local GICC/PPI/timer state, then while generic boot
    // publishes the bounded runtime owners needed before IRQ delivery.
    msr daifset, #0xf
    // TPIDR_EL1=0 is the explicit unbound state expected by the generic CPU
    // registry; CPU0 must not inherit a stale logical binding from firmware.
    msr TPIDR_EL1, xzr
    mov  x19, #0

    // Slot 0 is reserved for CPU0. Fill the complete linker-owned interval
    // [__boot_stack_bottom, __boot_cpu_stack_top) before assigning SP so the
    // high Rust side can later report its high-water mark.
    adrp x2, __boot_stack_bottom
    add  x2, x2, :lo12:__boot_stack_bottom
    adrp x3, __boot_cpu_stack_top
    add  x3, x3, :lo12:__boot_cpu_stack_top
    movz x4, #0xA5A5
    movk x4, #0xA5A5, lsl #16
    movk x4, #0xA5A5, lsl #32
    movk x4, #0xA5A5, lsl #48

fill_boot_stack:
    cmp  x2, x3
    b.hs boot_stack_ready
    str  x4, [x2], #8
    b    fill_boot_stack

boot_stack_ready:
    mov  sp, x3

    // Build the initial page tables in low-linked Rust. x0 receives the low
    // physical parameter address because BOOT_MMU_PARAMS resides in .boot.bss.
    // The builder reads the DTB from the bare-metal boot-protocol slot at the
    // beginning of RAM.
    adrp x0, BOOT_MMU_PARAMS
    add  x0, x0, :lo12:BOOT_MMU_PARAMS
    mov x1, x21             // Firmware DTB on Pi; ignored by QEMU's protocol.
    bl   boot_build_page_tables

    // Load the high entry as a literal near the transition point.
    // ldr =rust_entry places its high virtual address in x6. The address is not
    // dereferenced before MMU enable; it is retained only for the branch after
    // SCTLR_EL1.M=1. This is assembly-owned transition state rather than part
    // of BootMmuParams because the Rust builder does not own the high entry
    // symbol.
    adrp x20, BOOT_MMU_PARAMS
    add  x20, x20, :lo12:BOOT_MMU_PARAMS
    ldr  x6, =rust_entry

    // BootMmuParams layout, repr(C):
    //   +0  ttbr0      low PA L0 root for temporary identity mappings
    //   +8  ttbr1      low PA L0 root for the high direct map
    //   +16 tcr        TCR_EL1: 48-bit VA, 4 KiB granule, WBWA table walks
    //   +24 mair       MAIR_EL1: attr0 Device, attr1 Normal WB, attr2 Normal NC
    //   +32 high_stack_base high VA alias of bootstrap stack slot 0 base
    //   +40...         DTB/platform ranges, consumed later by high Rust code
    ldr  x1, [x20, #0]
    ldr  x2, [x20, #8]
    ldr  x3, [x20, #16]
    ldr  x4, [x20, #24]
    ldr  x5, [x20, #32]
    // high_stack_base names slot 0's bottom. Every stack is exactly 32 KiB,
    // so the top of slot i is base + (i + 1) * 2^15.
    add  x11, x19, #1
    add  x5, x5, x11, lsl #15
    b boot_program_mmu

// PSCI CPU_ON entry ABI for this kernel:
//   x0 = CPU0-selected bootstrap slot, supplied as PSCI context_id
//   MMU and caches are off
//   no generic logical binding exists yet
//   no secondary exception, GIC CPU-interface, timer, or scheduler state exists
//
// This path must remain self-contained in .boot.*. Any validation failure can
// only hard-park locally: CPU0 observes the missing readiness publication and
// turns it into a bounded startup timeout/fatal boot failure.
.global secondary_start
.type secondary_start, %function
secondary_start:
.if GENRT_PI_RUNTIME
    // armstub8 spin-table entry supplies no PSCI context_id. The validated
    // Pi topology uses logical slot == Aff0; CPU0 is rejected below. Firmware
    // leaves each Cortex-A53 coherent (SMPEN set), with MMU/data cache off.
    msr daifset, #0xf
    pi_enter_el1
    mrs x0, MPIDR_EL1
    and x0, x0, #0xff
.endif
    // Keep every asynchronous exception masked while high Rust installs local
    // vectors, registers this CPU with generic state, and initializes its
    // GICC/PPI and physical timer before entering the IRQ-enabled park loop.
    // Clear TPIDR_EL1 so generic current_cpu() cannot accidentally classify
    // this CPU as CPU0.
    msr  daifset, #0xf
    msr  TPIDR_EL1, xzr
    mov  x19, x0

    // Slot 0 belongs to the already-running boot CPU. A secondary presented
    // with zero is not allowed to share CPU0's stack or registration identity.
    cbz  x19, boot_terminal_park

    // Calculate the private low physical interval:
    //   bottom = stack_array_bottom + slot * 32 KiB
    //   top    = bottom + 32 KiB
    // The upper-bound check happens before the first store or SP update.
    adrp x2, __boot_stack_bottom
    add  x2, x2, :lo12:__boot_stack_bottom
    add  x2, x2, x19, lsl #15
    mov  x11, #1
    add  x3, x2, x11, lsl #15
    adrp x13, __boot_stack_top
    add  x13, x13, :lo12:__boot_stack_top
    cmp  x3, x13
    b.hi boot_terminal_park

    // Initialize only this CPU's interval. x2 is a fill cursor and x3 remains
    // the private low stack top installed into SP after the loop.
    movz x4, #0xA5A5
    movk x4, #0xA5A5, lsl #16
    movk x4, #0xA5A5, lsl #32
    movk x4, #0xA5A5, lsl #48
secondary_fill_boot_stack:
    cmp  x2, x3
    b.hs secondary_stack_ready
    str  x4, [x2], #8
    b secondary_fill_boot_stack

secondary_stack_ready:
    mov  sp, x3

    // BOOT_MMU_PARAMS and its page tables are boot-owned immutable storage.
    // The secondary needs boot TTBR0 only so instructions between SCTLR.M=1
    // and the high branch remain reachable at their low physical addresses.
    adrp x20, BOOT_MMU_PARAMS
    add  x20, x20, :lo12:BOOT_MMU_PARAMS
    ldr  x1, [x20, #0]

    // CPU0 stores the allocator-owned runtime TTBR1 root through the high
    // direct-map alias with Release ordering before PSCI CPU_ON. LDAR pairs
    // with that store, making the complete runtime tables visible here. Zero
    // means CPU0 has not published a usable handoff and execution must stop.
    adrp x8, SECONDARY_TTBR1
    add  x8, x8, :lo12:SECONDARY_TTBR1
    ldar x2, [x8]
    cbz  x2, boot_terminal_park

    // Reuse the exact translation attributes chosen by CPU0, then derive this
    // slot's high stack top from the common high alias base.
    ldr  x3, [x20, #16]
    ldr  x4, [x20, #24]
    ldr  x5, [x20, #32]
    add  x11, x19, #1
    add  x5, x5, x11, lsl #15

    // The literal contains a high virtual address. Do not dereference it while
    // the MMU is off; boot_program_mmu uses it only after translation is live.
    ldr  x6, =secondary_rust_entry

boot_program_mmu:
    // Callers arrive here only after preparing x1..x6 according to the common
    // contract documented above. Keeping one tail guarantees identical MAIR,
    // TCR, cache enable, TLB invalidation, and high-branch ordering on all CPUs.
    // Ordering matters: program MAIR/TCR first, TTBR0/TTBR1 second, then issue
    // the required barriers and TLBI. TTBR0 exists only for the low identity
    // execution window after MMU enable. TTBR1 contains the persistent high
    // direct map for kernel RAM, HVA aliases, and MMIO.
    msr  MAIR_EL1, x4
    msr  TCR_EL1, x3
    msr  TTBR0_EL1, x1
    msr  TTBR1_EL1, x2
    isb

    // Complete table-register publication before invalidating stale local
    // translations. At this point the CPU still executes through low TTBR0.
    dsb  sy
    tlbi vmalle1
    dsb  sy
    isb

    // Enable the MMU and caches:
    // - SCTLR_EL1.M  bit 0: stage-1 MMU enable.
    // - SCTLR_EL1.C  bit 2: data/unified cache enable.
    // - SCTLR_EL1.I  bit 12: instruction cache enable.
    // Preserve all other reset/reserved bits from the current SCTLR_EL1 value.
    mrs  x7, SCTLR_EL1
    orr  x7, x7, #1
    orr  x7, x7, #(1 << 2)
    orr  x7, x7, #(1 << 12)
    msr  SCTLR_EL1, x7
    isb

    // After the ISB, the MMU is active. The current low PC remains valid through
    // TTBR0. Switch SP to its high alias and branch to the high Rust entry via
    // x6.
    // High Rust receives x0 = low PA BootMmuParams and x1 = the
    // architecture-only bootstrap stack slot. The boot Rust entry requires
    // slot 0; the secondary Rust entry validates its nonzero slot against DTB
    // topology before generic registration.
    mov  sp, x5
    mov  x0, x20
    mov  x1, x19
    blr  x6

boot_terminal_park:
    // Contain malformed boot or secondary handoff, missing runtime TTBR1
    // publication, or an impossible return from high Rust. No shared state
    // is touched from this loop.
    wfe
    b boot_terminal_park

// Single boot-owned handoff word shared by CPU0 and secondary_start. It is not
// allocator-owned and is never reclaimed. CPU0 writes the runtime TTBR1 root
// before each CPU_ON; secondaries read it with LDAR before enabling the MMU.
.section .boot.bss.coordination, "aw", %nobits
.align 3
.global SECONDARY_TTBR1
SECONDARY_TTBR1:
    .skip 8
