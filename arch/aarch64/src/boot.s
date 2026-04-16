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
    // install exception vectors after .bss is initialized so Rust-side
    // diagnostics do not depend on pre-zeroed global state.
    adrp x6, __vectors
    add  x6, x6, :lo12:__vectors
    msr  VBAR_EL1, x6
    isb

    // save CurrentEL for debugging after .bss clear
    mrs  x6, CurrentEL
    adrp x7, BOOT_CURRENT_EL
    add  x7, x7, :lo12:BOOT_CURRENT_EL
    str  x6, [x7]

    bl rust_entry

4:
    wfe
    b 4b
