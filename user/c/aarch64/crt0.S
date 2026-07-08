.section .text.start, "ax"
.global _start
.extern main

.equ SYS_EXIT, 2

_start:
    ldr x0, [sp]
    add x1, sp, #8
    bl main
    mov x8, #SYS_EXIT
    svc #0

1:
    wfe
    b 1b
