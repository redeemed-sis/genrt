ENTRY(_start)

BOOT_PHYS_BASE = 0x40080000;
KERNEL_HVA_OFFSET = 0xffff000000000000;

SECTIONS
{
    . = BOOT_PHYS_BASE;
    __kernel_image_phys_start = .;
    __boot_phys_start = .;

    .boot.text : ALIGN(4K) {
        KEEP(*(.boot.text.entry))
        *(.boot.text .boot.text.*)
    }

    .boot.rodata : ALIGN(4K) {
        *(.boot.rodata .boot.rodata.*)
    }

    .boot.data : ALIGN(4K) {
        *(.boot.data .boot.data.*)
    }

    .boot.bss (NOLOAD) : ALIGN(4K) {
        *(.boot.bss .boot.bss.*)
    }

    .boot_stack (NOLOAD) : ALIGN(16) {
        __boot_stack_bottom = .;
        . = . + 0x8000;
        __boot_stack_top = .;
    }

    __boot_phys_end = .;
    __kernel_phys_start = ALIGN(__boot_phys_end, 4K);

    . = __kernel_phys_start + KERNEL_HVA_OFFSET;
    __kernel_virt_start = .;

    .text : AT(ADDR(.text) - KERNEL_HVA_OFFSET) ALIGN(4K) {
        *(.text .text.*)
    }

    .rodata : AT(ADDR(.rodata) - KERNEL_HVA_OFFSET) ALIGN(4K) {
        *(.rodata .rodata.*)
        . = ALIGN(8);
        __kernel_image_phys_start_value = .;
        QUAD(__kernel_image_phys_start);
        __kernel_image_phys_end_value = .;
        QUAD(__kernel_image_phys_end);
        __boot_stack_bottom_value = .;
        QUAD(__boot_stack_bottom);
        __boot_stack_top_value = .;
        QUAD(__boot_stack_top);
    }

    .data : AT(ADDR(.data) - KERNEL_HVA_OFFSET) ALIGN(4K) {
        *(.data .data.*)
    }

    .bss (NOLOAD) : AT(ADDR(.bss) - KERNEL_HVA_OFFSET) ALIGN(4K) {
        __bss_start = .;
        __bss_phys_start = LOADADDR(.bss);
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
        __bss_phys_end = LOADADDR(.bss) + SIZEOF(.bss);
    }

    __kernel_virt_end = .;
    __kernel_phys_end = __kernel_virt_end - KERNEL_HVA_OFFSET;
    __kernel_image_phys_end = __kernel_phys_end;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note .note.*)
    }
}
