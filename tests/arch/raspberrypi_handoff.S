// Emulator-only U-Boot substitute. QEMU raspi3b already supplies the secondary
// firmware spin loops. Configure its GPIO14/15 PL011 and hand off fixed test
// payload locations. This is not an alternative production board entry.
.text
.global _start
_start:
    ldr x4, =0x3f200004
    mov w5, #0x4000
    movk w5, #2, lsl #16
    str w5, [x4]            // GPIO14/15 ALT0.
    ldr x4, =0x3f201000
    mov w5, #0x70
    str w5, [x4, #0x2c]    // 8N1, FIFO enabled.
    mov w5, #0x301
    str w5, [x4, #0x30]    // UART/RX/TX enabled.
    mov x0, #0x07000000    // Test DTB.
    mov x1, #0
    mov x2, #0
    mov x3, #0
    mov x4, #0x02000000    // Unmodified production Pi Image.
    br x4
