.section .text
.global _start

_start:
    mov x0, xzr
    ldr x1, [x0]

1:
    wfe
    b 1b
