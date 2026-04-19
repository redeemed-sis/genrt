ENTRY(_start)

SECTIONS
{
    . = 0x40080000;
    __kernel_image_start = .;

    .text : ALIGN(4K) {
        KEEP(*(.text._start))
        *(.text .text.*)
    }

    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4K) {
        *(.data .data.*)
    }

    .bss (NOLOAD) : ALIGN(4K) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }
    __kernel_image_end = .;

    .boot_stack (NOLOAD) : ALIGN(16) {
        __boot_stack_bottom = .;
        . = . + 0x4000;
        __boot_stack_top = .;
    }

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note .note.*)
    }
}
