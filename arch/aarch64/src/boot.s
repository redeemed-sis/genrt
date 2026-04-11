.macro VEC_ENTRY target
    b \target
    .space 0x80 - 4
.endm

.align 11
.global __vectors
__vectors:
    // Current EL with SP0
    VEC_ENTRY trap_current_sp0_sync
    VEC_ENTRY trap_current_sp0_irq
    VEC_ENTRY trap_current_sp0_fiq
    VEC_ENTRY trap_current_sp0_serror

    // Current EL with SPx
    VEC_ENTRY trap_current_spx_sync
    VEC_ENTRY trap_current_spx_irq
    VEC_ENTRY trap_current_spx_fiq
    VEC_ENTRY trap_current_spx_serror

    // Lower EL using AArch64
    VEC_ENTRY trap_lower_el_aarch64_sync
    VEC_ENTRY trap_lower_el_aarch64_irq
    VEC_ENTRY trap_lower_el_aarch64_fiq
    VEC_ENTRY trap_lower_el_aarch64_serror

    // Lower EL using AArch32
    VEC_ENTRY trap_lower_el_aarch32_sync
    VEC_ENTRY trap_lower_el_aarch32_irq
    VEC_ENTRY trap_lower_el_aarch32_fiq
    VEC_ENTRY trap_lower_el_aarch32_serror

.section .text._start, "ax"
.global _start
.type _start, %function

_start:
    // x0 = DTB physical address from QEMU
    // park secondary CPUs for now
    mrs x1, mpidr_el1
    and x1, x1, #0xff
    cbz x1, 1f

0:
    wfe
    b 0b

1:
    // set up boot stack
    adrp x2, __boot_stack_top
    add  x2, x2, :lo12:__boot_stack_top
    mov  sp, x2

    // install exception vectors
    adrp x6, __vectors
    add  x6, x6, :lo12:__vectors
    msr  VBAR_EL1, x6
    isb

    // zero .bss
    adrp x3, __bss_start
    add  x3, x3, :lo12:__bss_start
    adrp x4, __bss_end
    add  x4, x4, :lo12:__bss_end
    mov  x5, xzr

2:
    cmp  x3, x4
    b.hs 3f
    str  x5, [x3], #8
    b    2b

3:
    // save CurrentEL for debugging after .bss clear
    mrs  x6, CurrentEL
    adrp x7, BOOT_CURRENT_EL
    add  x7, x7, :lo12:BOOT_CURRENT_EL
    str  x6, [x7]

    bl rust_entry

4:
    wfe
    b 4b

trap_current_sp0_sync:
trap_current_spx_sync:
    mrs x0, ESR_EL1
    mrs x1, FAR_EL1
    mrs x2, ELR_EL1
    bl  trap_record
1:
    wfe
    b 1b

trap_current_sp0_irq:
trap_current_spx_irq:
    // TrapFrame layout:
    // - x0..x30  at 0..240
    // - sp       at 248
    // - elr      at 256
    // - spsr     at 264
    sub sp, sp, #272
    stp x0,  x1,  [sp, #0]
    stp x2,  x3,  [sp, #16]
    stp x4,  x5,  [sp, #32]
    stp x6,  x7,  [sp, #48]
    stp x8,  x9,  [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    str x30, [sp, #240]

    add x9, sp, #272
    str x9, [sp, #248]
    mrs x9, ELR_EL1
    str x9, [sp, #256]
    mrs x9, SPSR_EL1
    str x9, [sp, #264]

    mov x0, sp
    bl irq_entry

    b trap_restore_and_eret

.global arch_enter_task_frame
.type arch_enter_task_frame, %function
arch_enter_task_frame:
    mov sp, x0

trap_restore_and_eret:
    mov x9, sp

    ldr x10, [x9, #248]
    ldr x11, [x9, #256]
    msr ELR_EL1, x11
    ldr x11, [x9, #264]
    msr SPSR_EL1, x11

    ldp x0,  x1,  [x9, #0]
    ldp x2,  x3,  [x9, #16]
    ldp x4,  x5,  [x9, #32]
    ldp x6,  x7,  [x9, #48]
    ldr x8,  [x9, #64]
    ldr x11, [x9, #88]
    ldp x12, x13, [x9, #96]
    ldp x14, x15, [x9, #112]
    ldp x16, x17, [x9, #128]
    ldp x18, x19, [x9, #144]
    ldp x20, x21, [x9, #160]
    ldp x22, x23, [x9, #176]
    ldp x24, x25, [x9, #192]
    ldp x26, x27, [x9, #208]
    ldp x28, x29, [x9, #224]
    ldr x30, [x9, #240]

    mov sp, x10
    ldr x10, [x9, #80]
    ldr x9,  [x9, #72]
    eret

trap_current_sp0_fiq:
trap_current_sp0_serror:
trap_current_spx_fiq:
trap_current_spx_serror:
trap_lower_el_aarch64_sync:
trap_lower_el_aarch64_irq:
trap_lower_el_aarch64_fiq:
trap_lower_el_aarch64_serror:
trap_lower_el_aarch32_sync:
trap_lower_el_aarch32_irq:
trap_lower_el_aarch32_fiq:
trap_lower_el_aarch32_serror:
    b trap_current_spx_sync
