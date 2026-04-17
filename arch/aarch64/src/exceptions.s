// AArch64 EL1 vector + trap frame save/restore path.
// Contract with Rust:
// - `irq_entry(*mut TrapFrame)` may update active frame for IRQ-return switching.
// - `sync_entry(vector_id, *mut TrapFrame)` handles controlled synchronous kernel traps.
// - `exception_entry(vector_id, *const TrapFrame)` is fatal and does not return.
//
// TrapFrame ABI (must match `trap_frame.rs`):
// - x0..x30: offsets 0..240
// - sp:      248
// - elr:     256
// - spsr:    264
// - size:    272 bytes
//
// Fatal exception contract:
// - before saving a fatal TrapFrame we switch to a dedicated emergency stack;
// - original x0/x1/sp are preserved through system registers and restored into the frame;
// - this avoids saving the fatal frame on a potentially corrupted current stack.

.set TF_X0,    0
.set TF_X1,    8
.set TF_X2,   16
.set TF_X3,   24
.set TF_X4,   32
.set TF_X5,   40
.set TF_X6,   48
.set TF_X7,   56
.set TF_X8,   64
.set TF_X9,   72
.set TF_X10,  80
.set TF_X11,  88
.set TF_X12,  96
.set TF_X13, 104
.set TF_X14, 112
.set TF_X15, 120
.set TF_X16, 128
.set TF_X17, 136
.set TF_X18, 144
.set TF_X19, 152
.set TF_X20, 160
.set TF_X21, 168
.set TF_X22, 176
.set TF_X23, 184
.set TF_X24, 192
.set TF_X25, 200
.set TF_X26, 208
.set TF_X27, 216
.set TF_X28, 224
.set TF_X29, 232
.set TF_X30, 240
.set TF_SP,  248
.set TF_ELR, 256
.set TF_SPSR,264
.set TF_SIZE,272

.set VECTOR_CURRENT_EL_SPX_SYNC, 4

.macro VEC_ENTRY target
    b \target
    .space 0x80 - 4
.endm

.macro SAVE_TRAPFRAME
    sub sp, sp, #TF_SIZE
    stp x0,  x1,  [sp, #TF_X0]
    stp x2,  x3,  [sp, #TF_X2]
    stp x4,  x5,  [sp, #TF_X4]
    stp x6,  x7,  [sp, #TF_X6]
    stp x8,  x9,  [sp, #TF_X8]
    stp x10, x11, [sp, #TF_X10]
    stp x12, x13, [sp, #TF_X12]
    stp x14, x15, [sp, #TF_X14]
    stp x16, x17, [sp, #TF_X16]
    stp x18, x19, [sp, #TF_X18]
    stp x20, x21, [sp, #TF_X20]
    stp x22, x23, [sp, #TF_X22]
    stp x24, x25, [sp, #TF_X24]
    stp x26, x27, [sp, #TF_X26]
    stp x28, x29, [sp, #TF_X28]
    str x30,      [sp, #TF_X30]

    add x9, sp, #TF_SIZE
    str x9, [sp, #TF_SP]
    mrs x9, ELR_EL1
    str x9, [sp, #TF_ELR]
    mrs x9, SPSR_EL1
    str x9, [sp, #TF_SPSR]
.endm

.macro SAVE_FATAL_TRAPFRAME
    sub sp, sp, #TF_SIZE
    mrs x0, TPIDR_EL1
    mrs x1, TPIDRRO_EL0
    stp x0,  x1,  [sp, #TF_X0]
    stp x2,  x3,  [sp, #TF_X2]
    stp x4,  x5,  [sp, #TF_X4]
    stp x6,  x7,  [sp, #TF_X6]
    stp x8,  x9,  [sp, #TF_X8]
    stp x10, x11, [sp, #TF_X10]
    stp x12, x13, [sp, #TF_X12]
    stp x14, x15, [sp, #TF_X14]
    stp x16, x17, [sp, #TF_X16]
    stp x18, x19, [sp, #TF_X18]
    stp x20, x21, [sp, #TF_X20]
    stp x22, x23, [sp, #TF_X22]
    stp x24, x25, [sp, #TF_X24]
    stp x26, x27, [sp, #TF_X26]
    stp x28, x29, [sp, #TF_X28]
    str x30,      [sp, #TF_X30]

    mrs x9, TPIDR_EL0
    str x9, [sp, #TF_SP]
    mrs x9, ELR_EL1
    str x9, [sp, #TF_ELR]
    mrs x9, SPSR_EL1
    str x9, [sp, #TF_SPSR]
.endm

.macro FATAL_VECTOR vec_id
    msr daifset, #0xf
    msr TPIDR_EL1, x0
    msr TPIDRRO_EL0, x1
    mov x0, sp
    msr TPIDR_EL0, x0

    adrp x0, __fatal_exception_stack_top
    add  x0, x0, :lo12:__fatal_exception_stack_top
    mov sp, x0

    SAVE_FATAL_TRAPFRAME
    mov x0, #\vec_id
    mov x1, sp
    bl exception_entry
1:
    wfe
    b 1b
.endm

.macro KERNEL_SYNC_VECTOR vec_id
    SAVE_TRAPFRAME
    mov x0, #\vec_id
    mov x1, sp
    bl sync_entry
    b trap_restore_and_eret
.endm

.section .text.exceptions, "ax"

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

trap_current_sp0_sync:
    FATAL_VECTOR 0

trap_current_sp0_irq:
    SAVE_TRAPFRAME
    mov x0, sp
    bl irq_entry
    b trap_restore_and_eret

trap_current_sp0_fiq:
    FATAL_VECTOR 2

trap_current_sp0_serror:
    FATAL_VECTOR 3

trap_current_spx_sync:
    KERNEL_SYNC_VECTOR VECTOR_CURRENT_EL_SPX_SYNC

trap_current_spx_irq:
    SAVE_TRAPFRAME
    mov x0, sp
    bl irq_entry
    b trap_restore_and_eret

trap_current_spx_fiq:
    FATAL_VECTOR 6

trap_current_spx_serror:
    FATAL_VECTOR 7

trap_lower_el_aarch64_sync:
    FATAL_VECTOR 8

trap_lower_el_aarch64_irq:
    FATAL_VECTOR 9

trap_lower_el_aarch64_fiq:
    FATAL_VECTOR 10

trap_lower_el_aarch64_serror:
    FATAL_VECTOR 11

trap_lower_el_aarch32_sync:
    FATAL_VECTOR 12

trap_lower_el_aarch32_irq:
    FATAL_VECTOR 13

trap_lower_el_aarch32_fiq:
    FATAL_VECTOR 14

trap_lower_el_aarch32_serror:
    FATAL_VECTOR 15

.global arch_enter_task_frame
.type arch_enter_task_frame, %function
arch_enter_task_frame:
    mov sp, x0

.global trap_restore_and_eret
trap_restore_and_eret:
    // Use x9 as frame base pointer and x10/x11 as temporaries while ELR/SPSR are restored.
    mov x9, sp

    ldr x10, [x9, #TF_SP]
    ldr x11, [x9, #TF_ELR]
    msr ELR_EL1, x11
    ldr x11, [x9, #TF_SPSR]
    msr SPSR_EL1, x11

    ldp x0,  x1,  [x9, #TF_X0]
    ldp x2,  x3,  [x9, #TF_X2]
    ldp x4,  x5,  [x9, #TF_X4]
    ldp x6,  x7,  [x9, #TF_X6]
    ldr x8,        [x9, #TF_X8]
    ldr x11,       [x9, #TF_X11]
    ldp x12, x13, [x9, #TF_X12]
    ldp x14, x15, [x9, #TF_X14]
    ldp x16, x17, [x9, #TF_X16]
    ldp x18, x19, [x9, #TF_X18]
    ldp x20, x21, [x9, #TF_X20]
    ldp x22, x23, [x9, #TF_X22]
    ldp x24, x25, [x9, #TF_X24]
    ldp x26, x27, [x9, #TF_X26]
    ldp x28, x29, [x9, #TF_X28]
    ldr x30,      [x9, #TF_X30]

    mov sp, x10
    ldr x10, [x9, #TF_X10]
    ldr x9,  [x9, #TF_X9]
    eret

.section .bss.fatal_exception_stack, "aw", %nobits
.align 12
__fatal_exception_stack:
    .space 4096
__fatal_exception_stack_top:
