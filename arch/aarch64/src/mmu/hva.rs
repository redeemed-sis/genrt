//! High virtual address/direct-map часть MMU.
//!
//! Этот модуль владеет моделью `HVA = PA + KERNEL_HVA_OFFSET`, AArch64
//! descriptor layout и post-init TTBR1 page-table mutation API. В PTE всегда
//! пишется physical address; HVA используется только для разыменования backing
//! memory page tables.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use kernel::memory::{PhysAddr, VirtAddr};

pub const KERNEL_HVA_OFFSET: usize = 0xffff_0000_0000_0000;

// Первый этап использует 2 MiB block descriptors на L2:
// L0 -> L1 table -> L2 table -> 2 MiB blocks.
pub(super) const BLOCK_SIZE_2M: usize = 2 * 1024 * 1024;
pub(super) const TABLE_ENTRIES: usize = 512;

// Биты AArch64 stage-1 descriptors. В PTE всегда хранится PA, не HVA.
pub(super) const DESC_VALID: u64 = 1 << 0;
pub(super) const DESC_TABLE: u64 = 1 << 1;
pub(super) const DESC_AF: u64 = 1 << 10;
const DESC_NG: u64 = 1 << 11;
pub(super) const DESC_PXN: u64 = 1 << 53;
pub(super) const DESC_UXN: u64 = 1 << 54;
const DESC_AP_USER: u64 = 1 << 6;
const DESC_AP_RO: u64 = 1 << 7;
pub(super) const DESC_SH_INNER: u64 = 0b11 << 8;
pub(super) const DESC_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

// AttrIndx в block descriptors:
//   0 -> Device-nGnRnE;
//   1 -> Normal Write-Back Read/Write Allocate;
//   2 -> Normal Non-Cacheable.
pub(super) const ATTR_DEVICE_NG_RN_E: u64 = 0;
pub(super) const ATTR_NORMAL_WB: u64 = 1;
pub(super) const ATTR_NORMAL_NC: u64 = 2;

#[repr(C, align(4096))]
pub(super) struct PageTable {
    entries: [u64; TABLE_ENTRIES],
}

impl PageTable {
    pub(super) const fn new() -> Self {
        Self {
            entries: [0; TABLE_ENTRIES],
        }
    }

    /// Очистить page table после MMU enable. `table` уже является HVA pointer.
    pub(super) unsafe fn clear_high(table: *mut Self) {
        let entries = unsafe { addr_of_mut!((*table).entries) as *mut u64 };
        let mut index = 0usize;
        while index < TABLE_ENTRIES {
            unsafe { write_volatile(entries.add(index), 0) };
            index += 1;
        }
    }

    /// Volatile read descriptor из high-mapped page table.
    pub(super) unsafe fn read(table: *mut Self, index: usize) -> u64 {
        let entries = unsafe { addr_of!((*table).entries) as *const u64 };
        unsafe { read_volatile(entries.add(index)) }
    }

    /// Volatile write descriptor в high-mapped page table.
    pub(super) unsafe fn write(table: *mut Self, index: usize, value: u64) {
        let entries = unsafe { addr_of_mut!((*table).entries) as *mut u64 };
        unsafe { write_volatile(entries.add(index), value) };
    }

    /// True when every descriptor is invalid. Used before reclaiming dynamic
    /// page-table frames on unmap.
    pub(super) unsafe fn is_empty_high(table: *mut Self) -> bool {
        let mut index = 0usize;
        while index < TABLE_ENTRIES {
            if unsafe { Self::read(table, index) } != 0 {
                return false;
            }
            index += 1;
        }
        true
    }
}

#[derive(Copy, Clone)]
struct MappedPageTable {
    table: *mut PageTable,
}

impl MappedPageTable {
    fn from_pa(pa: PhysAddr) -> Self {
        Self {
            table: phys_to_hva(pa) as *mut PageTable,
        }
    }

    fn clear(self) {
        unsafe { PageTable::clear_high(self.table) };
    }

    fn read(self, index: usize) -> u64 {
        unsafe { PageTable::read(self.table, index) }
    }

    fn write(self, index: usize, value: u64) {
        unsafe { PageTable::write(self.table, index, value) };
    }

    fn install_table(self, index: usize, child_pa: PhysAddr) {
        self.write(index, table_desc(child_pa));
    }

    fn is_empty(self) -> bool {
        unsafe { PageTable::is_empty_high(self.table) }
    }
}

static mut CURRENT_TTBR0_L0_PA: usize = 0;
static mut CURRENT_TTBR1_L0_PA: usize = 0;
static mut RUNTIME_TABLES_ACTIVE: bool = false;

/// Save physical roots of initial boot page tables for high-side VM API.
pub(super) unsafe fn save_boot_roots(ttbr0_l0: usize, ttbr1_l0: usize) {
    unsafe {
        set_current_ttbr_roots(ttbr0_l0, ttbr1_l0);
        set_runtime_tables_active(false);
    }
}

unsafe fn set_current_ttbr_roots(ttbr0_l0: usize, ttbr1_l0: usize) {
    unsafe {
        CURRENT_TTBR0_L0_PA = ttbr0_l0;
        CURRENT_TTBR1_L0_PA = ttbr1_l0;
    }
}

unsafe fn set_runtime_tables_active(active: bool) {
    unsafe {
        RUNTIME_TABLES_ACTIVE = active;
    }
}

fn runtime_tables_active() -> bool {
    unsafe { RUNTIME_TABLES_ACTIVE }
}

fn require_runtime_tables() -> Result<(), VmError> {
    if runtime_tables_active() {
        Ok(())
    } else {
        Err(VmError::NotInitialized)
    }
}

/// Current physical root of TTBR0 L0 table.
fn ttbr0_l0_pa() -> usize {
    unsafe { CURRENT_TTBR0_L0_PA }
}

/// Current physical root of TTBR1 L0 table.
fn ttbr1_l0_pa() -> usize {
    unsafe { CURRENT_TTBR1_L0_PA }
}

/// Const helper для MMIO base constants: `HVA = PA + KERNEL_HVA_OFFSET`.
pub const fn phys_to_hva_const(pa: usize) -> usize {
    pa.wrapping_add(KERNEL_HVA_OFFSET)
}

/// Runtime PA -> high direct-map VA conversion.
#[inline(always)]
pub fn phys_to_hva(pa: usize) -> usize {
    pa.wrapping_add(KERNEL_HVA_OFFSET)
}

/// Runtime high direct-map VA -> PA conversion.
#[inline(always)]
pub fn hva_to_phys(va: usize) -> usize {
    va.wrapping_sub(KERNEL_HVA_OFFSET)
}

/// Memory attribute, выбираемый через AttrIndx в descriptor.
#[repr(u32)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum VmMemoryAttr {
    /// Device-nGnRnE: строго упорядоченный MMIO без gather/reorder/early ack.
    DeviceNgRnE = 0,
    /// Normal WB RA/WA: обычная кэшируемая RAM.
    NormalWriteBack = 1,
    /// Normal Non-Cacheable: зарезервировано для специальных regions.
    NormalNonCacheable = 2,
}

/// Portable flags уровня VM API. Они транслируются в AArch64 descriptor bits:
/// - WRITE отсутствует => AP[2] read-only;
/// - USER присутствует => AP[1] EL0 allowed;
/// - GLOBAL отсутствует => nG;
/// - EXECUTE отсутствует => PXN|UXN.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct VmFlags(u64);

impl VmFlags {
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const USER: Self = Self(1 << 3);
    pub const GLOBAL: Self = Self(1 << 4);

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn contains(self, rhs: Self) -> bool {
        (self.0 & rhs.0) == rhs.0
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum VmError {
    NotInitialized,
    InvalidRange,
    NotAligned,
    NotKernelVa,
    AlreadyMapped,
    MissingMapping,
    OutOfFrames,
    Unsupported,
}

pub(super) fn vm_error_code(err: VmError) -> u64 {
    match err {
        VmError::NotInitialized => 1,
        VmError::InvalidRange => 2,
        VmError::NotAligned => 3,
        VmError::NotKernelVa => 4,
        VmError::AlreadyMapped => 5,
        VmError::MissingMapping => 6,
        VmError::OutOfFrames => 7,
        VmError::Unsupported => 8,
    }
}

pub(super) fn vm_attr_from_code(code: u32) -> Result<VmMemoryAttr, VmError> {
    match code {
        0 => Ok(VmMemoryAttr::DeviceNgRnE),
        1 => Ok(VmMemoryAttr::NormalWriteBack),
        2 => Ok(VmMemoryAttr::NormalNonCacheable),
        _ => Err(VmError::Unsupported),
    }
}

/// Direct-map PA -> VA helper для arch-side callers.
pub(super) fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    phys_to_hva(pa)
}

/// Быстрый direct-map VA -> PA. Не ходит в page tables и проверяет только high
/// direct-map half-space.
pub(super) fn virt_to_phys_direct(va: VirtAddr) -> Option<PhysAddr> {
    (va >= KERNEL_HVA_OFFSET).then_some(hva_to_phys(va))
}

/// Page-table translation для TTBR1 kernel VA.
///
/// Сейчас поддерживаются только L2 block descriptors, поэтому результат
/// вычисляется как `block_pa + offset_in_2MiB`.
pub(super) fn translate_kernel_va(va: VirtAddr) -> Option<PhysAddr> {
    if !is_kernel_va(va) {
        return None;
    }

    let root = ttbr1_l0_pa();
    if root == 0 {
        return None;
    }

    let l0 = MappedPageTable::from_pa(root);
    let l0e = l0.read(l0_index(va));
    if !is_table_desc(l0e) {
        return None;
    }
    let l1 = MappedPageTable::from_pa(desc_pa(l0e));
    let l1e = l1.read(l1_index(va));
    if !is_table_desc(l1e) {
        return None;
    }
    let l2 = MappedPageTable::from_pa(desc_pa(l1e));
    let l2e = l2.read(l2_index(va));
    if !is_block_desc(l2e) {
        return None;
    }

    Some(desc_pa(l2e) + (va & (BLOCK_SIZE_2M - 1)))
}

/// Map 2 MiB-aligned TTBR1 kernel region.
///
/// PTE получает physical address `pa`; page-table memory разыменовывается через
/// high direct-map alias. При необходимости новые L1/L2 tables берутся из
/// generic frame allocator как physical frames.
pub(super) unsafe fn map_kernel_region(
    va: VirtAddr,
    pa: PhysAddr,
    size: usize,
    attr: VmMemoryAttr,
    flags: VmFlags,
) -> Result<(), VmError> {
    require_runtime_tables()?;
    validate_kernel_region(va, pa, size)?;

    let mut offset = 0usize;
    while offset < size {
        let current_va = va + offset;
        let current_pa = pa + offset;
        let l2 = ensure_l2_table(current_va)?;
        let index = l2_index(current_va);
        let old = l2.read(index);
        if old & DESC_VALID != 0 {
            return Err(VmError::AlreadyMapped);
        }
        l2.write(index, block_desc(current_pa, attr_to_index(attr), flags));
        offset += BLOCK_SIZE_2M;
    }

    unsafe { flush_tlb_all() };
    Ok(())
}

/// Unmap 2 MiB-aligned TTBR1 kernel region.
pub(super) unsafe fn unmap_kernel_region(va: VirtAddr, size: usize) -> Result<(), VmError> {
    require_runtime_tables()?;
    validate_kernel_va_size(va, size)?;

    let mut offset = 0usize;
    while offset < size {
        let current_va = va + offset;
        let Some(l2) = existing_l2_table(current_va) else {
            return Err(VmError::MissingMapping);
        };
        let index = l2_index(current_va);
        let old = l2.read(index);
        if !is_block_desc(old) {
            return Err(VmError::MissingMapping);
        }
        l2.write(index, 0);
        reclaim_empty_tables_for_va(current_va);
        offset += BLOCK_SIZE_2M;
    }

    unsafe { flush_tlb_all() };
    Ok(())
}

/// Change protection flags for an existing 2 MiB block mapping.
///
/// Для break-before-make сначала сбрасываем descriptor, делаем TLBI, затем
/// записываем новый descriptor с тем же PA и AttrIndx.
pub(super) unsafe fn protect_kernel_region(
    va: VirtAddr,
    size: usize,
    flags: VmFlags,
) -> Result<(), VmError> {
    require_runtime_tables()?;
    validate_kernel_va_size(va, size)?;

    let mut offset = 0usize;
    while offset < size {
        let current_va = va + offset;
        let Some(l2) = existing_l2_table(current_va) else {
            return Err(VmError::MissingMapping);
        };
        let index = l2_index(current_va);
        let old = l2.read(index);
        if !is_block_desc(old) {
            return Err(VmError::MissingMapping);
        }

        let attr = ((old >> 2) & 0b111) as u64;
        let pa = desc_pa(old);
        l2.write(index, 0);
        unsafe { flush_tlb_all() };
        l2.write(index, block_desc(pa, attr, flags));
        offset += BLOCK_SIZE_2M;
    }

    unsafe { flush_tlb_all() };
    Ok(())
}

/// Удалить temporary TTBR0 identity mapping.
///
/// Безопасно вызывать только после high jump, high SP, high VBAR, high MMIO и
/// после перевода всех physical dereference boundaries на HVA.
pub(super) unsafe fn drop_boot_identity_mapping() -> Result<(), VmError> {
    let root = ttbr0_l0_pa();
    if root == 0 {
        return Ok(());
    }

    MappedPageTable::from_pa(root).clear();
    unsafe { flush_tlb_all() };
    Ok(())
}

/// Replace boot-owned TTBR tables with allocator-owned runtime tables.
///
/// This runs after `kernel::memory::init()`, when the generic frame allocator is
/// ready. From this point on, TTBR1 intermediate tables are normal physical
/// frames and can be reclaimed by `unmap_kernel_region()`. TTBR0 is explicitly
/// cleared until userspace/per-process address spaces exist.
pub(super) unsafe fn switch_to_runtime_kernel_tables() -> Result<(), VmError> {
    if runtime_tables_active() {
        return Ok(());
    }

    let platform = RuntimePlatformMap::from_platform_info(
        crate::platform::info().ok_or(VmError::NotInitialized)?,
    )?;
    let mut l0_frame = OwnedTableFrame::alloc()?;
    let mut l1_frame = OwnedTableFrame::alloc()?;
    let mut l2_gic_frame = OwnedTableFrame::alloc()?;
    let mut l2_uart_frame = if l1_index(phys_to_hva(platform.gic.start))
        == l1_index(phys_to_hva(platform.uart.start))
    {
        None
    } else {
        Some(OwnedTableFrame::alloc()?)
    };
    let mut l2_ram_frame = OwnedTableFrame::alloc()?;

    let l0 = l0_frame.table();
    let l1 = l1_frame.table();
    let l2_gic = l2_gic_frame.table();
    let l2_uart = l2_uart_frame
        .as_ref()
        .map(OwnedTableFrame::table)
        .unwrap_or(l2_gic);
    let l2_ram = l2_ram_frame.table();

    l0.clear();
    l1.clear();
    l2_gic.clear();
    if let Some(frame) = l2_uart_frame.as_ref() {
        frame.table().clear();
    }
    l2_ram.clear();

    let high_ram = phys_to_hva(platform.ram.start);
    let high_gic = phys_to_hva(platform.gic.start);
    let high_uart = phys_to_hva(platform.uart.start);
    l0.install_table(l0_index(high_ram), l1_frame.pa());
    l1.install_table(l1_index(high_ram), l2_ram_frame.pa());
    l1.install_table(l1_index(high_gic), l2_gic_frame.pa());
    if let Some(frame) = l2_uart_frame.as_ref() {
        l1.install_table(l1_index(high_uart), frame.pa());
    }
    map_l2_2m_blocks(
        l2_ram,
        high_ram,
        platform.ram.start,
        platform.ram.size,
        VmMemoryAttr::NormalWriteBack,
        VmFlags::from_bits((1 << 1) | (1 << 2) | (1 << 4)),
    );
    map_l2_2m_blocks(
        l2_gic,
        high_gic,
        platform.gic.start,
        platform.gic.size,
        VmMemoryAttr::DeviceNgRnE,
        VmFlags::from_bits((1 << 1) | (1 << 4)),
    );
    map_l2_2m_blocks(
        l2_uart,
        high_uart,
        platform.uart.start,
        platform.uart.size,
        VmMemoryAttr::DeviceNgRnE,
        VmFlags::from_bits((1 << 1) | (1 << 4)),
    );

    unsafe { install_runtime_ttbrs(l0_frame.pa()) };
    let l0_pa = l0_frame.release();
    l1_frame.release();
    l2_gic_frame.release();
    if let Some(frame) = l2_uart_frame.as_mut() {
        frame.release();
    }
    l2_ram_frame.release();
    unsafe {
        set_current_ttbr_roots(0, l0_pa);
        set_runtime_tables_active(true);
    }
    Ok(())
}

#[derive(Copy, Clone)]
struct RuntimePlatformMap {
    ram: AlignedMapRange,
    gic: AlignedMapRange,
    uart: AlignedMapRange,
}

impl RuntimePlatformMap {
    fn from_platform_info(platform: &crate::platform::PlatformInfo) -> Result<Self, VmError> {
        let ram = aligned_range_from_device(platform.ram)?;
        let uart = aligned_range_from_device(platform.uart)?;
        let gicd = aligned_range_from_device(platform.gic_distributor)?;
        let gicc = aligned_range_from_device(platform.gic_cpu_interface)?;

        Ok(Self {
            ram,
            gic: gicd.cover(gicc)?,
            uart,
        })
    }
}

#[derive(Copy, Clone)]
struct AlignedMapRange {
    start: PhysAddr,
    size: usize,
}

impl AlignedMapRange {
    fn cover(self, rhs: Self) -> Result<Self, VmError> {
        let start = self.start.min(rhs.start);
        let end = self
            .end()
            .ok_or(VmError::InvalidRange)?
            .max(rhs.end().ok_or(VmError::InvalidRange)?);
        Ok(Self {
            start,
            size: end.checked_sub(start).ok_or(VmError::InvalidRange)?,
        })
    }

    fn end(self) -> Option<PhysAddr> {
        self.start.checked_add(self.size)
    }
}

fn aligned_range_from_device(
    range: crate::platform::DeviceRange,
) -> Result<AlignedMapRange, VmError> {
    if !range.is_present() {
        return Err(VmError::InvalidRange);
    }

    let start = align_down_2m(range.start as usize);
    let raw_end = (range.start as usize)
        .checked_add(range.size as usize)
        .ok_or(VmError::InvalidRange)?;
    let end = align_up_2m(raw_end)?;
    Ok(AlignedMapRange {
        start,
        size: end.checked_sub(start).ok_or(VmError::InvalidRange)?,
    })
}

/// High-side table descriptor builder для post-init VM API.
#[inline(always)]
const fn table_desc(pa: usize) -> u64 {
    (pa as u64 & DESC_ADDR_MASK) | DESC_VALID | DESC_TABLE
}

/// High-side L2 block descriptor builder.
///
/// Биты доступа формируются из `VmFlags`, но PA остается физическим адресом.
#[inline(always)]
const fn block_desc(pa: usize, attr_index: u64, flags: VmFlags) -> u64 {
    let mut desc = (pa as u64 & DESC_ADDR_MASK) | DESC_VALID | DESC_AF | (attr_index << 2);
    if attr_index == ATTR_NORMAL_WB || attr_index == ATTR_NORMAL_NC {
        desc |= DESC_SH_INNER;
    }
    if !flags.contains(VmFlags::WRITE) {
        desc |= DESC_AP_RO;
    }
    if flags.contains(VmFlags::USER) {
        desc |= DESC_AP_USER;
    }
    if !flags.contains(VmFlags::GLOBAL) {
        desc |= DESC_NG;
    }
    if !flags.contains(VmFlags::EXECUTE) {
        desc |= DESC_PXN | DESC_UXN;
    }
    desc
}

/// L0 index for 4 KiB granule, 48-bit VA: bits [47:39].
#[inline(always)]
pub(super) const fn l0_index(va: usize) -> usize {
    (va >> 39) & 0x1ff
}

/// L1 index for 4 KiB granule: bits [38:30].
#[inline(always)]
pub(super) const fn l1_index(va: usize) -> usize {
    (va >> 30) & 0x1ff
}

/// L2 index for 2 MiB block mappings: bits [29:21].
#[inline(always)]
pub(super) const fn l2_index(va: usize) -> usize {
    (va >> 21) & 0x1ff
}

fn validate_kernel_region(va: VirtAddr, pa: PhysAddr, size: usize) -> Result<(), VmError> {
    validate_kernel_va_size(va, size)?;
    if pa & (BLOCK_SIZE_2M - 1) != 0 {
        return Err(VmError::NotAligned);
    }
    Ok(())
}

// VM API первого этапа deliberately narrow: только kernel VA, только 2 MiB
// alignment/size. Это держит page-table mutation auditable до появления
// полноценного VM subsystem.
fn validate_kernel_va_size(va: VirtAddr, size: usize) -> Result<(), VmError> {
    if size == 0 {
        return Err(VmError::InvalidRange);
    }
    if !is_kernel_va(va) {
        return Err(VmError::NotKernelVa);
    }
    if va & (BLOCK_SIZE_2M - 1) != 0 || size & (BLOCK_SIZE_2M - 1) != 0 {
        return Err(VmError::NotAligned);
    }
    Ok(())
}

const fn align_down_2m(addr: usize) -> usize {
    addr & !(BLOCK_SIZE_2M - 1)
}

fn align_up_2m(addr: usize) -> Result<usize, VmError> {
    if addr & (BLOCK_SIZE_2M - 1) == 0 {
        Ok(addr)
    } else {
        align_down_2m(addr)
            .checked_add(BLOCK_SIZE_2M)
            .ok_or(VmError::InvalidRange)
    }
}

#[inline(always)]
fn is_kernel_va(va: VirtAddr) -> bool {
    va >= KERNEL_HVA_OFFSET
}

/// Найти или создать L2 table для TTBR1 VA.
///
/// Новые page-table frames выделяются как PA из generic frame allocator и
/// чистятся через HVA alias. В descriptors записывается именно PA.
fn ensure_l2_table(va: VirtAddr) -> Result<MappedPageTable, VmError> {
    let root = ttbr1_l0_pa();
    if root == 0 {
        return Err(VmError::NotInitialized);
    }

    let l0 = MappedPageTable::from_pa(root);
    let l0e = l0.read(l0_index(va));
    let l1 = if is_table_desc(l0e) {
        MappedPageTable::from_pa(desc_pa(l0e))
    } else if l0e & DESC_VALID == 0 {
        let pa = alloc_table_frame()?;
        let table = MappedPageTable::from_pa(pa);
        table.clear();
        l0.install_table(l0_index(va), pa);
        table
    } else {
        return Err(VmError::Unsupported);
    };

    let l1e = l1.read(l1_index(va));
    let l2 = if is_table_desc(l1e) {
        MappedPageTable::from_pa(desc_pa(l1e))
    } else if l1e & DESC_VALID == 0 {
        let pa = match alloc_table_frame() {
            Ok(pa) => pa,
            Err(err) => {
                reclaim_empty_tables_for_va(va);
                return Err(err);
            }
        };
        let table = MappedPageTable::from_pa(pa);
        table.clear();
        l1.install_table(l1_index(va), pa);
        table
    } else {
        return Err(VmError::Unsupported);
    };
    Ok(l2)
}

/// Reclaim empty allocator-owned intermediate tables for one VA path.
///
/// This relies on the post-memory-init invariant that TTBR1 has already been
/// switched away from boot `.boot.bss.pt` tables to frames owned by the generic
/// frame allocator.
fn reclaim_empty_tables_for_va(va: VirtAddr) {
    let root = ttbr1_l0_pa();
    if root == 0 {
        return;
    }

    let l0 = MappedPageTable::from_pa(root);
    let l0_index = l0_index(va);
    let l0e = l0.read(l0_index);
    if !is_table_desc(l0e) {
        return;
    }

    let l1_pa = desc_pa(l0e);
    let l1 = MappedPageTable::from_pa(l1_pa);
    let l1_index = l1_index(va);
    let l1e = l1.read(l1_index);
    if is_table_desc(l1e) {
        let l2_pa = desc_pa(l1e);
        let l2 = MappedPageTable::from_pa(l2_pa);
        if l2.is_empty() {
            l1.write(l1_index, 0);
            kernel::memory::free_frame(l2_pa);
        }
    }

    if l1.is_empty() {
        l0.write(l0_index, 0);
        kernel::memory::free_frame(l1_pa);
    }
}

/// Найти существующую L2 table без allocation.
fn existing_l2_table(va: VirtAddr) -> Option<MappedPageTable> {
    let root = ttbr1_l0_pa();
    if root == 0 {
        return None;
    }

    let l0 = MappedPageTable::from_pa(root);
    let l0e = l0.read(l0_index(va));
    if !is_table_desc(l0e) {
        return None;
    }

    let l1 = MappedPageTable::from_pa(desc_pa(l0e));
    let l1e = l1.read(l1_index(va));
    is_table_desc(l1e).then(|| MappedPageTable::from_pa(desc_pa(l1e)))
}

/// Получить physical frame для новой page table.
///
/// Generic allocator не знает про MMU: он возвращает PA, а разыменование
/// происходит позже через `MappedPageTable`.
fn alloc_table_frame() -> Result<PhysAddr, VmError> {
    kernel::memory::alloc_frame().ok_or(VmError::OutOfFrames)
}

struct OwnedTableFrame {
    pa: PhysAddr,
}

impl OwnedTableFrame {
    fn alloc() -> Result<Self, VmError> {
        let pa = alloc_table_frame()?;
        Ok(Self { pa })
    }

    fn pa(&self) -> PhysAddr {
        self.pa
    }

    fn table(&self) -> MappedPageTable {
        MappedPageTable::from_pa(self.pa)
    }

    fn release(&mut self) -> PhysAddr {
        let pa = self.pa;
        self.pa = 0;
        pa
    }
}

impl Drop for OwnedTableFrame {
    fn drop(&mut self) {
        if self.pa != 0 {
            kernel::memory::free_frame(self.pa);
        }
    }
}

#[inline(always)]
fn is_table_desc(desc: u64) -> bool {
    (desc & (DESC_VALID | DESC_TABLE)) == (DESC_VALID | DESC_TABLE)
}

#[inline(always)]
fn is_block_desc(desc: u64) -> bool {
    (desc & DESC_VALID) != 0 && (desc & DESC_TABLE) == 0
}

#[inline(always)]
fn desc_pa(desc: u64) -> PhysAddr {
    (desc & DESC_ADDR_MASK) as usize
}

fn attr_to_index(attr: VmMemoryAttr) -> u64 {
    match attr {
        VmMemoryAttr::DeviceNgRnE => ATTR_DEVICE_NG_RN_E,
        VmMemoryAttr::NormalWriteBack => ATTR_NORMAL_WB,
        VmMemoryAttr::NormalNonCacheable => ATTR_NORMAL_NC,
    }
}

/// Map a run of 2 MiB block descriptors into an L2 table.
///
/// This is deliberately not a `MappedPageTable` method: only L2 tables may hold
/// block descriptors in the current 4 KiB-granule setup, while the wrapper is
/// also used for L0/L1 tables.
fn map_l2_2m_blocks(
    l2: MappedPageTable,
    va_base: VirtAddr,
    pa_base: PhysAddr,
    size: usize,
    attr: VmMemoryAttr,
    flags: VmFlags,
) {
    let mut offset = 0usize;
    while offset < size {
        let va = va_base + offset;
        let pa = pa_base + offset;
        l2.write(l2_index(va), block_desc(pa, attr_to_index(attr), flags));
        offset += BLOCK_SIZE_2M;
    }
}

/// Install allocator-owned TTBR1 and clear TTBR0 for the kernel-only stage.
unsafe fn install_runtime_ttbrs(ttbr1_l0_pa: PhysAddr) {
    unsafe {
        dsb_sy();
        clear_ttbr0();
        set_ttbr1(ttbr1_l0_pa);
        flush_tlb_all();
    }
}

unsafe fn set_ttbr0(root_pa: PhysAddr) {
    unsafe {
        core::arch::asm!(
            "msr ttbr0_el1, {root}",
            "isb",
            root = in(reg) root_pa as u64,
            options(nostack, preserves_flags)
        );
    }
}

unsafe fn clear_ttbr0() {
    unsafe {
        core::arch::asm!(
            "msr ttbr0_el1, xzr",
            "isb",
            options(nostack, preserves_flags)
        );
    }
}

unsafe fn set_ttbr1(root_pa: PhysAddr) {
    unsafe {
        core::arch::asm!(
            "msr ttbr1_el1, {root}",
            "isb",
            root = in(reg) root_pa as u64,
            options(nostack, preserves_flags)
        );
    }
}

unsafe fn clear_ttbr1() {
    unsafe {
        core::arch::asm!(
            "msr ttbr1_el1, xzr",
            "isb",
            options(nostack, preserves_flags)
        );
    }
}

/// Глобальная TLB invalidation sequence для текущего single-core этапа.
///
/// На SMP это место надо заменить на shootdown protocol; пока все mappings
/// меняются на одном core и не из IRQ fast path.
unsafe fn flush_tlb_all() {
    unsafe {
        core::arch::asm!(
            "dsb sy",
            "tlbi vmalle1",
            "dsb sy",
            "isb",
            options(nostack, preserves_flags)
        );
    }
}

unsafe fn dsb_sy() {
    unsafe {
        core::arch::asm!("dsb sy", options(nostack, preserves_flags));
    }
}
