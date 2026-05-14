// Ранний AArch64 trampoline для стратегии:
//   low-linked trampoline + high-linked kernel loaded low.
//
// Важные условия до включения MMU:
// - PC и SP должны быть low physical/identity addresses.
// - Нельзя обращаться к high-linked symbols как к указателям.
// - Нельзя чистить main .bss через __bss_start/__bss_end: эти symbols уже high VA.
// - Нельзя ставить VBAR_EL1 на high __vectors до включения TTBR1 mapping.
//
// Регистровый договор этого файла:
// - x0 на входе не является частью нашего bare-metal boot protocol.
// - x20: low PA указатель на BOOT_MMU_PARAMS в .boot.bss.
// - x1..x6 после boot_build_page_tables(): MAIR/TCR/TTBR/SP/entry параметры.
//
// boot_build_page_tables() лежит в .boot.text и заполняет только low .boot.bss
// page tables. После SCTLR_EL1.M=1 текущая инструкция еще исполняется через
// TTBR0 identity mapping; явный blr x6 переводит PC в high-half rust_entry.
.section .boot.text.entry, "ax"
.global _start
.type _start, %function

_start:
    // QEMU virt может стартовать несколько CPU. genrt пока single-core, поэтому
    // все secondary CPUs паркуются в WFE. Primary CPU имеет MPIDR_EL1.Aff0 == 0.
    mrs x1, mpidr_el1
    and x1, x1, #0xff
    cbz x1, 1f

0:
    wfe
    b 0b

1:
    // Заполняем low bootstrap stack шаблоном 0xA5 до его первого использования.
    // Позже high kernel читает этот же физический диапазон через HVA alias и
    // измеряет stack high-water mark.
    adrp x2, __boot_stack_bottom
    add  x2, x2, :lo12:__boot_stack_bottom
    adrp x3, __boot_stack_top
    add  x3, x3, :lo12:__boot_stack_top
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
    // До MMU стек остается low/identity. __boot_stack_top лежит в .boot_stack,
    // то есть имеет low VMA=LMA и пригоден для прямого использования.
    mov  sp, x3

    // Сборка initial page tables на Rust стороне. x0 получает low PA params,
    // потому что BOOT_MMU_PARAMS находится в .boot.bss. DTB берется самим
    // builder'ом из bare-metal boot-protocol слота в начале RAM.
    adrp x0, BOOT_MMU_PARAMS
    add  x0, x0, :lo12:BOOT_MMU_PARAMS
    bl   boot_build_page_tables

    // Загружаем high entry как literal рядом с точкой перехода:
    // ldr =rust_entry кладет high VA в x6. До включения MMU мы не
    // dereference'им этот адрес, а только держим его для branch после
    // SCTLR_EL1.M=1. Это assembly-owned transition state, не часть
    // BootMmuParams, потому что Rust builder не владеет high entry symbol.
    adrp x20, BOOT_MMU_PARAMS
    add  x20, x20, :lo12:BOOT_MMU_PARAMS
    ldr  x6, =rust_entry

    // BootMmuParams layout, repr(C):
    //   +0  ttbr0      low PA L0 root для temporary identity mappings
    //   +8  ttbr1      low PA L0 root для high direct map
    //   +16 tcr        TCR_EL1: 48-bit VA, 4 KiB granule, WBWA table walks
    //   +24 mair       MAIR_EL1: attr0 Device, attr1 Normal WB, attr2 Normal NC
    //   +32 high_stack high VA alias bootstrap stack top
    //   +40...         DTB/platform ranges, consumed later by high Rust code
    ldr  x1, [x20, #0]
    ldr  x2, [x20, #8]
    ldr  x3, [x20, #16]
    ldr  x4, [x20, #24]
    ldr  x5, [x20, #32]

    // Порядок важен: сначала MAIR/TCR, затем TTBR0/TTBR1, потом barriers/TLBI.
    // TTBR0 нужен только для low identity execution window после включения MMU.
    // TTBR1 содержит постоянный high direct map для kernel/HVA/MMIO.
    msr  MAIR_EL1, x4
    msr  TCR_EL1, x3
    msr  TTBR0_EL1, x1
    msr  TTBR1_EL1, x2
    isb

    dsb  sy
    tlbi vmalle1
    dsb  sy
    isb

    // Включаем MMU и кэши:
    // - SCTLR_EL1.M  bit 0: stage-1 MMU enable.
    // - SCTLR_EL1.C  bit 2: data/unified cache enable.
    // - SCTLR_EL1.I  bit 12: instruction cache enable.
    // Остальные reset/reserved bits сохраняются из текущего SCTLR_EL1.
    mrs  x7, SCTLR_EL1
    orr  x7, x7, #1
    orr  x7, x7, #(1 << 2)
    orr  x7, x7, #(1 << 12)
    msr  SCTLR_EL1, x7
    isb

    // После ISB MMU включен. Текущий PC все еще low и валиден через TTBR0.
    // Переключаем SP на high alias и прыгаем в high rust_entry через x6.
    // rust_entry получает:
    // - x0 = low PA BootMmuParams, который high code прочитает через HVA.
    mov  sp, x5
    mov  x0, x20
    blr  x6

4:
    wfe
    b 4b
