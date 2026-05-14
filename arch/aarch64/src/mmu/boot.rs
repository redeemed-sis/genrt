//! Low `.boot.*` часть MMU bring-up.
//!
//! Этот модуль исполняется до включения MMU и поэтому не должен зависеть от
//! heap, formatting, panic path или high-linked helper calls. Он только строит
//! initial page tables в `.boot.bss` и передает assembly trampoline значения для
//! MAIR/TCR/TTBR/SP.

use core::ptr::addr_of_mut;

use super::hva::{
    ATTR_DEVICE_NG_RN_E, ATTR_NORMAL_NC, ATTR_NORMAL_WB, BLOCK_SIZE_2M, DESC_ADDR_MASK, DESC_AF,
    DESC_PXN, DESC_SH_INNER, DESC_TABLE, DESC_UXN, DESC_VALID, KERNEL_HVA_OFFSET, PageTable,
    TABLE_ENTRIES, l0_index, l1_index, l2_index, save_boot_roots,
};

use crate::platform::{BootDeviceRange, BootPlatformInfo, parse_boot_platform};

// MAIR_EL1 encoding:
// - Device-nGnRnE = 0x00 для MMIO;
// - Normal WB RA/WA = 0xff для RAM/kernel/heap;
// - Normal NC = 0x44 зарезервирован для будущих mappings.
const MAIR_DEVICE_NG_RN_E: u64 = 0x00;
const MAIR_NORMAL_WB_RA_WA: u64 = 0xff;
const MAIR_NORMAL_NC: u64 = 0x44;

// TCR_EL1 для первого режима:
// - T0SZ/T1SZ = 16 => 48-bit VA space for TTBR0 and TTBR1;
// - TG0/TG1 = 4 KiB granule (TG1 encoding для 4 KiB равен 0b10);
// - table walks are Inner Shareable, WBWA;
// - IPS = 40-bit PA, достаточно для QEMU virt bring-up.
const TCR_T0SZ_48BIT: u64 = 16;
const TCR_T1SZ_48BIT: u64 = 16 << 16;
const TCR_IRGN0_WBWA: u64 = 0b01 << 8;
const TCR_ORGN0_WBWA: u64 = 0b01 << 10;
const TCR_SH0_INNER: u64 = 0b11 << 12;
const TCR_TG0_4K: u64 = 0b00 << 14;
const TCR_IRGN1_WBWA: u64 = 0b01 << 24;
const TCR_ORGN1_WBWA: u64 = 0b01 << 26;
const TCR_SH1_INNER: u64 = 0b11 << 28;
const TCR_TG1_4K: u64 = 0b10 << 30;
const TCR_IPS_40BIT: u64 = 0b010 << 32;

const BOOT_MAIR: u64 = (MAIR_DEVICE_NG_RN_E << (ATTR_DEVICE_NG_RN_E * 8))
    | (MAIR_NORMAL_WB_RA_WA << (ATTR_NORMAL_WB * 8))
    | (MAIR_NORMAL_NC << (ATTR_NORMAL_NC * 8));
const BOOT_TCR: u64 = TCR_T0SZ_48BIT
    | TCR_T1SZ_48BIT
    | TCR_IRGN0_WBWA
    | TCR_ORGN0_WBWA
    | TCR_SH0_INNER
    | TCR_TG0_4K
    | TCR_IRGN1_WBWA
    | TCR_ORGN1_WBWA
    | TCR_SH1_INNER
    | TCR_TG1_4K
    | TCR_IPS_40BIT;

/// Параметры, которые low Rust builder передает assembly trampoline.
///
/// Layout фиксирован `repr(C)` и используется в `boot.s` по offsets:
/// `0, 8, 16, 24, 32`. Поля после `high_stack` читает уже high Rust side.
#[repr(C)]
pub struct BootMmuParams {
    /// Low PA корневой L0 table для TTBR0_EL1 temporary identity mappings.
    pub ttbr0: u64,
    /// Low PA корневой L0 table для TTBR1_EL1 high direct map.
    pub ttbr1: u64,
    /// Полное значение TCR_EL1.
    pub tcr: u64,
    /// Полное значение MAIR_EL1.
    pub mair: u64,
    /// High VA alias `__boot_stack_top`.
    pub high_stack: u64,
    /// Platform-owned DTB/RAM/MMIO ranges discovered before MMU enable.
    pub platform: crate::platform::BootPlatformParams,
}

impl BootMmuParams {
    pub const fn zeroed() -> Self {
        Self {
            ttbr0: 0,
            ttbr1: 0,
            tcr: 0,
            mair: 0,
            high_stack: 0,
            platform: crate::platform::BootPlatformParams::zeroed(),
        }
    }
}

const BOOT_PLATFORM_PARAMS_OFFSET: usize = core::mem::offset_of!(BootMmuParams, platform);

#[unsafe(no_mangle)]
#[unsafe(link_section = ".boot.bss.params")]
pub static mut BOOT_MMU_PARAMS: BootMmuParams = BootMmuParams::zeroed();

// Все initial tables лежат в `.boot.bss.pt`: до MMU это low PA pointers, после
// MMU тот же physical storage доступен через high direct-map alias.
#[unsafe(link_section = ".boot.bss.pt")]
static mut TTBR0_L0: PageTable = PageTable::new();
#[unsafe(link_section = ".boot.bss.pt")]
static mut TTBR0_L1: PageTable = PageTable::new();
#[unsafe(link_section = ".boot.bss.pt")]
static mut TTBR0_L2_RAM: PageTable = PageTable::new();

#[unsafe(link_section = ".boot.bss.pt")]
static mut TTBR1_L0: PageTable = PageTable::new();
#[unsafe(link_section = ".boot.bss.pt")]
static mut TTBR1_L1: PageTable = PageTable::new();
#[unsafe(link_section = ".boot.bss.pt")]
static mut TTBR1_L2_RAM: PageTable = PageTable::new();
#[unsafe(link_section = ".boot.bss.pt")]
static mut TTBR1_L2_GIC: PageTable = PageTable::new();
#[unsafe(link_section = ".boot.bss.pt")]
static mut TTBR1_L2_UART: PageTable = PageTable::new();

unsafe extern "C" {
    static __boot_stack_top: u8;
    static __kernel_image_phys_start: u8;
    static __kernel_image_phys_end: u8;
}

/// Low-linked page-table builder.
///
/// Вызывается из `_start` до включения MMU. Функция должна оставаться пригодной
/// для исполнения при identity-only addressing:
/// - не использовать heap/log/format/panic;
/// - не читать high virtual symbols как память;
/// - писать только в low `.boot.bss` page tables и `BootMmuParams`.
///
/// Low parser сразу читает DTB из platform-specific bare-metal слота,
/// извлекает RAM, UART и GIC `reg` ranges, после чего page tables содержат:
/// - TTBR0 temporary identity maps для kernel image/DTB execution window;
/// - TTBR1 high direct-map для RAM;
/// - TTBR1 high Device mappings для UART/GIC.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".boot.text")]
pub unsafe extern "C" fn boot_build_page_tables(params: *mut BootMmuParams) {
    let ttbr0_l0 = addr_of_mut!(TTBR0_L0);
    let ttbr0_l1 = addr_of_mut!(TTBR0_L1);
    let ttbr0_l2_ram = addr_of_mut!(TTBR0_L2_RAM);
    let ttbr1_l0 = addr_of_mut!(TTBR1_L0);
    let ttbr1_l1 = addr_of_mut!(TTBR1_L1);
    let ttbr1_l2_ram = addr_of_mut!(TTBR1_L2_RAM);
    let ttbr1_l2_gic = addr_of_mut!(TTBR1_L2_GIC);
    let ttbr1_l2_uart = addr_of_mut!(TTBR1_L2_UART);
    let image_start = align_down_2m(core::ptr::addr_of!(__kernel_image_phys_start) as usize);
    let image_end = align_up_2m(core::ptr::addr_of!(__kernel_image_phys_end) as usize);
    let image_size = image_end.wrapping_sub(image_start);
    let mut platform = BootPlatformInfo::zeroed();
    unsafe { parse_boot_platform(crate::platform::qemu::BOOT_DTB_PA, &mut platform) };
    if !platform_info_complete(&platform) {
        crate::platform::qemu::apply_fallback_platform_info(&mut platform);
    }
    let ram_map = aligned_range_or_image(platform.ram, image_start, image_size);
    let gic_map = cover_ranges(platform.gic_distributor, platform.gic_cpu_interface);
    let uart_map = platform.uart;

    unsafe {
        // Boot tables находятся в NOBITS `.boot.bss`, но явно чистим их здесь:
        // early assembly больше не чистит main `.bss`, а эти tables должны быть
        // детерминированно нулевыми до записи descriptors.
        PageTable::clear_low(ttbr0_l0);
        PageTable::clear_low(ttbr0_l1);
        PageTable::clear_low(ttbr0_l2_ram);
        PageTable::clear_low(ttbr1_l0);
        PageTable::clear_low(ttbr1_l1);
        PageTable::clear_low(ttbr1_l2_ram);
        PageTable::clear_low(ttbr1_l2_gic);
        PageTable::clear_low(ttbr1_l2_uart);

        // TTBR0: low identity window. Он нужен только для переходного периода:
        // после SCTLR_EL1.M=1 текущий PC еще low, пока boot.s не сделает branch
        // в high `rust_entry`.
        PageTable::install_table_low(ttbr0_l0, l0_index(image_start), ttbr0_l1 as usize);
        PageTable::install_table_low(ttbr0_l1, l1_index(image_start), ttbr0_l2_ram as usize);
        PageTable::map_blocks_low(
            ttbr0_l2_ram,
            image_start,
            image_start,
            image_size,
            ATTR_NORMAL_WB,
            true,
        );

        // TTBR1: high direct-map RAM из DTB. Это нужно сразу после high jump:
        // frame allocator пишет metadata во free physical frames через HVA.
        let high_ram = boot_phys_to_hva(ram_map.start);
        PageTable::install_table_low(ttbr1_l0, l0_index(high_ram), ttbr1_l1 as usize);
        PageTable::install_table_low(ttbr1_l1, l1_index(high_ram), ttbr1_l2_ram as usize);
        PageTable::map_blocks_low(
            ttbr1_l2_ram,
            high_ram,
            ram_map.start,
            ram_map.size,
            ATTR_NORMAL_WB,
            true,
        );

        if platform.dtb_pa != 0 && (platform.dtb_pa < image_start || platform.dtb_pa >= image_end) {
            let dtb_block = align_down_2m(platform.dtb_pa);
            let high_dtb = boot_phys_to_hva(dtb_block);
            PageTable::map_blocks_low(
                ttbr0_l2_ram,
                dtb_block,
                dtb_block,
                BLOCK_SIZE_2M,
                ATTR_NORMAL_WB,
                false,
            );
            PageTable::map_blocks_low(
                ttbr1_l2_ram,
                high_dtb,
                dtb_block,
                BLOCK_SIZE_2M,
                ATTR_NORMAL_WB,
                false,
            );
        }

        if gic_map.is_present() {
            let high_gic = boot_phys_to_hva(gic_map.start);
            let gic_l1_index = l1_index(high_gic);
            let ram_l1_index = l1_index(high_ram);
            let gic_l2 = if gic_l1_index == ram_l1_index {
                ttbr1_l2_ram
            } else {
                PageTable::install_table_low(ttbr1_l1, gic_l1_index, ttbr1_l2_gic as usize);
                ttbr1_l2_gic
            };
            PageTable::map_blocks_low(
                gic_l2,
                high_gic,
                gic_map.start,
                gic_map.size,
                ATTR_DEVICE_NG_RN_E,
                false,
            );
        }

        if uart_map.is_present() {
            let high_uart = boot_phys_to_hva(uart_map.start);
            let uart_l1_index = l1_index(high_uart);
            let ram_l1_index = l1_index(high_ram);
            let gic_l1_index = l1_index(boot_phys_to_hva(gic_map.start));
            let uart_l2 = if uart_l1_index == ram_l1_index {
                ttbr1_l2_ram
            } else if gic_map.is_present() && uart_l1_index == gic_l1_index {
                ttbr1_l2_gic
            } else {
                PageTable::install_table_low(ttbr1_l1, uart_l1_index, ttbr1_l2_uart as usize);
                ttbr1_l2_uart
            };
            PageTable::map_blocks_low(
                uart_l2,
                high_uart,
                uart_map.start,
                uart_map.size,
                ATTR_DEVICE_NG_RN_E,
                false,
            );
        }

        // Assembly trampoline читает эти значения и пишет их в системные
        // регистры. High `rust_entry` остается assembly-owned literal'ом рядом
        // с самим branch, поэтому не входит в BootMmuParams.
        write_param(params, 0, ttbr0_l0 as u64);
        write_param(params, 1, ttbr1_l0 as u64);
        write_param(params, 2, BOOT_TCR);
        write_param(params, 3, BOOT_MAIR);
        write_param(
            params,
            4,
            (core::ptr::addr_of!(__boot_stack_top) as usize).wrapping_add(KERNEL_HVA_OFFSET) as u64,
        );
        crate::platform::write_boot_platform_params(
            core::ptr::addr_of_mut!((*params).platform),
            &platform,
        );
    }
}

/// High-side фиксация boot parameters после прыжка в `rust_entry`.
///
/// `params_pa` все еще physical address `.boot.bss.params`; после включения MMU
/// high code читает его через direct-map alias и сохраняет root PAs для VM API.
pub unsafe fn init_from_boot_params(params_pa: usize) {
    let params = super::hva::phys_to_hva(params_pa) as *const BootMmuParams;
    unsafe {
        save_boot_roots(
            core::ptr::addr_of!((*params).ttbr0).read_volatile() as usize,
            core::ptr::addr_of!((*params).ttbr1).read_volatile() as usize,
        );
    }
}

pub(crate) fn platform_params_from_boot_params(
    params_pa: usize,
) -> *const crate::platform::BootPlatformParams {
    let platform_params_pa = params_pa.wrapping_add(BOOT_PLATFORM_PARAMS_OFFSET);
    super::hva::phys_to_hva(platform_params_pa) as *const crate::platform::BootPlatformParams
}

/// Записать одно поле BootMmuParams по фиксированному 8-byte offset.
#[unsafe(link_section = ".boot.text")]
unsafe fn write_param(params: *mut BootMmuParams, index: usize, value: u64) {
    unsafe { boot_store64((params as usize).wrapping_add(index.wrapping_mul(8)), value) };
}

#[unsafe(link_section = ".boot.text")]
const fn align_down_2m(addr: usize) -> usize {
    addr & !(BLOCK_SIZE_2M - 1)
}

#[unsafe(link_section = ".boot.text")]
const fn align_up_2m(addr: usize) -> usize {
    align_down_2m(addr.wrapping_add(BLOCK_SIZE_2M - 1))
}

#[unsafe(link_section = ".boot.text")]
const fn aligned_range_or_image(
    range: BootDeviceRange,
    image_start: usize,
    image_size: usize,
) -> BootDeviceRange {
    if range.is_present() {
        align_range_2m(range)
    } else {
        BootDeviceRange {
            start: image_start,
            size: image_size,
        }
    }
}

#[unsafe(link_section = ".boot.text")]
const fn cover_ranges(lhs: BootDeviceRange, rhs: BootDeviceRange) -> BootDeviceRange {
    if !lhs.is_present() {
        return rhs;
    }
    if !rhs.is_present() {
        return lhs;
    }
    let lhs = align_range_2m(lhs);
    let rhs = align_range_2m(rhs);
    let start = if lhs.start < rhs.start {
        lhs.start
    } else {
        rhs.start
    };
    let lhs_end = lhs.start.wrapping_add(lhs.size);
    let rhs_end = rhs.start.wrapping_add(rhs.size);
    let end = if lhs_end > rhs_end { lhs_end } else { rhs_end };
    BootDeviceRange {
        start,
        size: end.wrapping_sub(start),
    }
}

#[unsafe(link_section = ".boot.text")]
const fn align_range_2m(range: BootDeviceRange) -> BootDeviceRange {
    let start = align_down_2m(range.start);
    let end = align_up_2m(range.start.wrapping_add(range.size));
    BootDeviceRange {
        start,
        size: end.wrapping_sub(start),
    }
}

#[unsafe(link_section = ".boot.text")]
const fn platform_info_complete(info: &BootPlatformInfo) -> bool {
    info.ram.is_present()
        && info.uart.is_present()
        && info.gic_distributor.is_present()
        && info.gic_cpu_interface.is_present()
}

/// Минимальная store primitive для low boot path.
///
/// Не используем `write_volatile()` здесь: в debug builds он может подтянуть
/// high-linked runtime checks. Inline `str` оставляет `.boot.text` автономным.
#[unsafe(link_section = ".boot.text")]
unsafe fn boot_store64(addr: usize, value: u64) {
    unsafe {
        core::arch::asm!(
            "str {value}, [{addr}]",
            addr = in(reg) addr,
            value = in(reg) value,
            options(nostack, preserves_flags)
        );
    }
}

/// Low-local `PA + HVA_OFFSET`, чтобы boot builder не вызывал high helper.
#[unsafe(link_section = ".boot.text")]
const fn boot_phys_to_hva(pa: usize) -> usize {
    pa.wrapping_add(KERNEL_HVA_OFFSET)
}

impl PageTable {
    #[unsafe(link_section = ".boot.text")]
    unsafe fn clear_low(table: *mut Self) {
        let entries = table as usize;
        let mut index = 0usize;
        while index < TABLE_ENTRIES {
            unsafe { boot_store64(entries.wrapping_add(index.wrapping_mul(8)), 0) };
            index = index.wrapping_add(1);
        }
    }

    /// Low `.boot.text` variant: parent/child are physical identity pointers.
    /// Использует только low-local descriptor builder, чтобы не получить thunk в
    /// high `.text` до включения MMU.
    #[unsafe(link_section = ".boot.text")]
    unsafe fn install_table_low(parent: *mut Self, index: usize, child_pa: usize) {
        let entries = parent as usize;
        unsafe {
            boot_store64(
                entries.wrapping_add(index.wrapping_mul(8)),
                boot_table_desc(child_pa),
            )
        };
    }

    /// Записать последовательность L2 block descriptors в low boot table.
    ///
    /// `va_base` может быть low identity VA или high direct-map VA; `pa_base`
    /// всегда physical. `executable=true` используется для RAM window, false для
    /// MMIO, где PXN|UXN обязательны.
    #[unsafe(link_section = ".boot.text")]
    unsafe fn map_blocks_low(
        l2: *mut Self,
        va_base: usize,
        pa_base: usize,
        size: usize,
        attr_index: u64,
        executable: bool,
    ) {
        let entries = l2 as usize;
        let mut offset = 0usize;
        while offset < size {
            let va = va_base.wrapping_add(offset);
            let pa = pa_base.wrapping_add(offset);
            unsafe {
                boot_store64(
                    entries.wrapping_add(l2_index(va).wrapping_mul(8)),
                    boot_block_desc(pa, attr_index, executable),
                );
            }
            offset = offset.wrapping_add(BLOCK_SIZE_2M);
        }
    }
}

/// Low-local table descriptor:
/// bit0 Valid=1, bit1 Table=1, address bits содержат child table PA.
#[unsafe(link_section = ".boot.text")]
const fn boot_table_desc(pa: usize) -> u64 {
    (pa as u64 & DESC_ADDR_MASK) | DESC_VALID | DESC_TABLE
}

/// Low-local L2 block descriptor для initial mappings.
///
/// Для RAM выставляем Inner Shareable. Для non-executable regions добавляем
/// PXN|UXN. Access flag ставится сразу, чтобы не ловить access flag fault.
#[unsafe(link_section = ".boot.text")]
const fn boot_block_desc(pa: usize, attr_index: u64, executable: bool) -> u64 {
    let mut desc = (pa as u64 & DESC_ADDR_MASK) | DESC_VALID | DESC_AF | (attr_index << 2);
    if attr_index == ATTR_NORMAL_WB || attr_index == ATTR_NORMAL_NC {
        desc |= DESC_SH_INNER;
    }
    if !executable {
        desc |= DESC_PXN | DESC_UXN;
    }
    desc
}
