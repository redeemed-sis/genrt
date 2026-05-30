.section .text
.global _start

.equ SYS_WRITE, 1
.equ SYS_EXIT, 2
.equ STDOUT, 1

_start:
    mov x8, #SYS_WRITE
    mov x0, #STDOUT
    adr x1, msg
    mov x2, #msg_len
    svc #0

    mov x8, #SYS_EXIT
    mov x0, #0
    svc #0

1:
    wfe
    b 1b

msg:
    .ascii "hello from EL0\n"
.equ msg_len, . - msg
