//! Minimal low `.boot.text` DTB parser.
//!
//! This parser exists only for MMU bring-up. It runs before the MMU, before the
//! heap, and before panic/logging are usable, so it deliberately understands only
//! the small subset needed to build initial mappings: RAM, PL011, and GICv2
//! `reg` ranges.

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;
const FDT_HEADER_SIZE: usize = 40;

#[unsafe(link_section = ".boot.rodata")]
static PROP_ADDRESS_CELLS: [u8; 14] = *b"#address-cells";
#[unsafe(link_section = ".boot.rodata")]
static PROP_SIZE_CELLS: [u8; 11] = *b"#size-cells";
#[unsafe(link_section = ".boot.rodata")]
static PROP_COMPATIBLE: [u8; 10] = *b"compatible";
#[unsafe(link_section = ".boot.rodata")]
static PROP_DEVICE_TYPE: [u8; 11] = *b"device_type";
#[unsafe(link_section = ".boot.rodata")]
static PROP_REG: [u8; 3] = *b"reg";
#[unsafe(link_section = ".boot.rodata")]
static DEVICE_TYPE_MEMORY: [u8; 6] = *b"memory";
#[unsafe(link_section = ".boot.rodata")]
static COMPAT_PL011: [u8; 9] = *b"arm,pl011";
#[unsafe(link_section = ".boot.rodata")]
static COMPAT_GICV2: [u8; 18] = *b"arm,cortex-a15-gic";

#[derive(Copy, Clone)]
pub(crate) struct BootDeviceRange {
    pub start: usize,
    pub size: usize,
}

impl BootDeviceRange {
    #[unsafe(link_section = ".boot.text")]
    pub const fn is_present(self) -> bool {
        self.start != 0 && self.size != 0
    }
}

#[derive(Copy, Clone)]
pub(crate) struct BootPlatformInfo {
    pub dtb_pa: usize,
    pub dtb_size: usize,
    pub ram: BootDeviceRange,
    pub uart: BootDeviceRange,
    pub gic_distributor: BootDeviceRange,
    pub gic_cpu_interface: BootDeviceRange,
}

impl BootPlatformInfo {
    #[unsafe(link_section = ".boot.text")]
    pub(crate) const fn zeroed() -> Self {
        Self {
            dtb_pa: 0,
            dtb_size: 0,
            ram: BootDeviceRange { start: 0, size: 0 },
            uart: BootDeviceRange { start: 0, size: 0 },
            gic_distributor: BootDeviceRange { start: 0, size: 0 },
            gic_cpu_interface: BootDeviceRange { start: 0, size: 0 },
        }
    }

    #[unsafe(link_section = ".boot.text")]
    fn reset(&mut self, dtb_pa: usize) {
        self.dtb_pa = dtb_pa;
        self.dtb_size = 0;
        self.ram.start = 0;
        self.ram.size = 0;
        self.uart.start = 0;
        self.uart.size = 0;
        self.gic_distributor.start = 0;
        self.gic_distributor.size = 0;
        self.gic_cpu_interface.start = 0;
        self.gic_cpu_interface.size = 0;
    }
}

#[derive(Copy, Clone)]
struct NodeState {
    is_memory: bool,
    is_uart: bool,
    is_gic: bool,
    reg0: BootDeviceRange,
    reg1: BootDeviceRange,
    reg_count: u8,
}

impl NodeState {
    #[unsafe(link_section = ".boot.text")]
    const fn zeroed() -> Self {
        Self {
            is_memory: false,
            is_uart: false,
            is_gic: false,
            reg0: BootDeviceRange { start: 0, size: 0 },
            reg1: BootDeviceRange { start: 0, size: 0 },
            reg_count: 0,
        }
    }

    #[unsafe(link_section = ".boot.text")]
    fn reset(&mut self) {
        self.is_memory = false;
        self.is_uart = false;
        self.is_gic = false;
        self.reg0.start = 0;
        self.reg0.size = 0;
        self.reg1.start = 0;
        self.reg1.size = 0;
        self.reg_count = 0;
    }

    #[unsafe(link_section = ".boot.text")]
    fn push_reg(&mut self, range: BootDeviceRange) {
        if !range.is_present() {
            return;
        }
        if self.reg_count == 0 {
            self.reg0 = range;
            self.reg_count = 1;
        } else if self.reg_count == 1 {
            self.reg1 = range;
            self.reg_count = 2;
        }
    }
}

/// Parse the bare-metal DTB slot and extract the ranges required for initial
/// page tables.
///
/// On parse failure the returned structure simply has empty ranges. The high
/// side will fail validation later; the low path cannot safely log or panic.
#[unsafe(link_section = ".boot.text")]
pub(crate) unsafe fn parse_boot_platform(dtb_pa: usize, out: &mut BootPlatformInfo) {
    out.reset(dtb_pa);
    let Some(total_size) = (unsafe { fdt_total_size(dtb_pa) }) else {
        return;
    };
    out.dtb_size = total_size;

    let Some(off_struct) = (unsafe { read_be_u32_at(dtb_pa, total_size, 8) }) else {
        return;
    };
    let Some(off_strings) = (unsafe { read_be_u32_at(dtb_pa, total_size, 12) }) else {
        return;
    };
    let Some(size_struct) = (unsafe { read_be_u32_at(dtb_pa, total_size, 36) }) else {
        return;
    };

    let struct_start = off_struct as usize;
    let strings_start = off_strings as usize;
    let struct_end = if size_struct == 0 {
        total_size
    } else {
        let end = struct_start.wrapping_add(size_struct as usize);
        if end < struct_start || end > total_size {
            return;
        }
        end
    };
    if strings_start >= total_size || struct_start >= total_size {
        return;
    }

    let mut depth = 0usize;
    let mut cursor = struct_start;
    let mut root_addr_cells = 2u32;
    let mut root_size_cells = 1u32;
    let mut current = NodeState::zeroed();

    while can_add_within(cursor, 4, struct_end) {
        let Some(token) = (unsafe { read_be_u32_at(dtb_pa, total_size, cursor) }) else {
            return;
        };
        cursor = cursor.wrapping_add(4);

        match token {
            FDT_BEGIN_NODE => {
                let Some(name_end) = (unsafe { scan_nul(dtb_pa, total_size, cursor) }) else {
                    return;
                };
                cursor = align4(name_end.wrapping_add(1));
                depth = depth.wrapping_add(1);
                if depth == 2 {
                    current.reset();
                }
            }
            FDT_END_NODE => {
                if depth == 0 {
                    return;
                }
                if depth == 2 {
                    if current.is_memory && !out.ram.is_present() {
                        out.ram = current.reg0;
                    }
                    if current.is_uart && !out.uart.is_present() {
                        out.uart = current.reg0;
                    }
                    if current.is_gic
                        && !out.gic_distributor.is_present()
                        && !out.gic_cpu_interface.is_present()
                    {
                        out.gic_distributor = current.reg0;
                        out.gic_cpu_interface = current.reg1;
                    }
                }
                depth = depth.wrapping_sub(1);
            }
            FDT_PROP => {
                if depth == 0 {
                    return;
                }
                let Some(len) = (unsafe { read_be_u32_at(dtb_pa, total_size, cursor) }) else {
                    return;
                };
                let Some(nameoff) =
                    (unsafe { read_be_u32_at(dtb_pa, total_size, cursor.wrapping_add(4)) })
                else {
                    return;
                };
                cursor = cursor.wrapping_add(8);
                let value_off = cursor;
                let value_len = len as usize;
                let value_end = value_off.wrapping_add(value_len);
                if value_end < value_off || value_end > total_size {
                    return;
                }

                if depth == 1
                    && unsafe {
                        prop_name_eq(
                            dtb_pa,
                            total_size,
                            strings_start,
                            nameoff as usize,
                            PROP_ADDRESS_CELLS.as_ptr(),
                            PROP_ADDRESS_CELLS.len(),
                        )
                    }
                {
                    if let Some(cells) = unsafe { read_be_u32_at(dtb_pa, total_size, value_off) } {
                        root_addr_cells = cells;
                    }
                } else if depth == 1
                    && unsafe {
                        prop_name_eq(
                            dtb_pa,
                            total_size,
                            strings_start,
                            nameoff as usize,
                            PROP_SIZE_CELLS.as_ptr(),
                            PROP_SIZE_CELLS.len(),
                        )
                    }
                {
                    if let Some(cells) = unsafe { read_be_u32_at(dtb_pa, total_size, value_off) } {
                        root_size_cells = cells;
                    }
                } else if depth == 2
                    && unsafe {
                        prop_name_eq(
                            dtb_pa,
                            total_size,
                            strings_start,
                            nameoff as usize,
                            PROP_REG.as_ptr(),
                            PROP_REG.len(),
                        )
                    }
                {
                    unsafe {
                        parse_reg_property(
                            dtb_pa,
                            total_size,
                            value_off,
                            value_len,
                            root_addr_cells,
                            root_size_cells,
                            &mut current,
                        );
                    }
                } else if depth == 2
                    && unsafe {
                        prop_name_eq(
                            dtb_pa,
                            total_size,
                            strings_start,
                            nameoff as usize,
                            PROP_DEVICE_TYPE.as_ptr(),
                            PROP_DEVICE_TYPE.len(),
                        )
                    }
                {
                    current.is_memory = unsafe {
                        value_is_cstr(
                            dtb_pa,
                            total_size,
                            value_off,
                            value_len,
                            DEVICE_TYPE_MEMORY.as_ptr(),
                            DEVICE_TYPE_MEMORY.len(),
                        )
                    };
                } else if depth == 2
                    && unsafe {
                        prop_name_eq(
                            dtb_pa,
                            total_size,
                            strings_start,
                            nameoff as usize,
                            PROP_COMPATIBLE.as_ptr(),
                            PROP_COMPATIBLE.len(),
                        )
                    }
                {
                    if unsafe {
                        compatible_contains(
                            dtb_pa,
                            total_size,
                            value_off,
                            value_len,
                            COMPAT_PL011.as_ptr(),
                            COMPAT_PL011.len(),
                        )
                    } {
                        current.is_uart = true;
                    }
                    if unsafe {
                        compatible_contains(
                            dtb_pa,
                            total_size,
                            value_off,
                            value_len,
                            COMPAT_GICV2.as_ptr(),
                            COMPAT_GICV2.len(),
                        )
                    } {
                        current.is_gic = true;
                    }
                }
                cursor = align4(value_end);
            }
            FDT_NOP => {}
            FDT_END => return,
            _ => return,
        }
    }
}

#[unsafe(link_section = ".boot.text")]
unsafe fn fdt_total_size(dtb_pa: usize) -> Option<usize> {
    let magic = match unsafe { read_be_u32_raw(dtb_pa) } {
        Some(value) => value,
        None => return None,
    };
    if magic != FDT_MAGIC {
        return None;
    }
    let total_size = match unsafe { read_be_u32_raw(dtb_pa.wrapping_add(4)) } {
        Some(value) => value as usize,
        None => return None,
    };
    if total_size >= FDT_HEADER_SIZE {
        Some(total_size)
    } else {
        None
    }
}

#[unsafe(link_section = ".boot.text")]
unsafe fn parse_reg_property(
    dtb_pa: usize,
    total_size: usize,
    value_off: usize,
    value_len: usize,
    addr_cells: u32,
    size_cells: u32,
    node: &mut NodeState,
) {
    let addr_bytes = (addr_cells as usize).wrapping_mul(4);
    let size_bytes = (size_cells as usize).wrapping_mul(4);
    let stride = addr_bytes.wrapping_add(size_bytes);
    if stride == 0 || addr_cells > 2 || size_cells > 2 {
        return;
    }

    let mut offset = 0usize;
    while can_add_within(offset, stride, value_len) && node.reg_count < 2 {
        let Some(address) = (unsafe {
            read_cells(
                dtb_pa,
                total_size,
                value_off.wrapping_add(offset),
                addr_cells,
            )
        }) else {
            return;
        };
        let Some(size) = (unsafe {
            read_cells(
                dtb_pa,
                total_size,
                value_off.wrapping_add(offset).wrapping_add(addr_bytes),
                size_cells,
            )
        }) else {
            return;
        };
        node.push_reg(BootDeviceRange {
            start: address as usize,
            size: size as usize,
        });
        offset = offset.wrapping_add(stride);
    }
}

#[unsafe(link_section = ".boot.text")]
unsafe fn read_cells(dtb_pa: usize, total_size: usize, offset: usize, cells: u32) -> Option<u64> {
    let mut value = 0u64;
    let mut index = 0u32;
    while index < cells {
        let cell_off = offset.wrapping_add((index as usize).wrapping_mul(4));
        let cell = match unsafe { read_be_u32_at(dtb_pa, total_size, cell_off) } {
            Some(value) => value,
            None => return None,
        };
        value = (value << 32) | cell as u64;
        index = index.wrapping_add(1);
    }
    Some(value)
}

#[unsafe(link_section = ".boot.text")]
unsafe fn prop_name_eq(
    dtb_pa: usize,
    total_size: usize,
    strings_start: usize,
    nameoff: usize,
    expected: *const u8,
    expected_len: usize,
) -> bool {
    let start = strings_start.wrapping_add(nameoff);
    if start < strings_start {
        return false;
    }
    unsafe { cstr_eq(dtb_pa, total_size, start, expected, expected_len) }
}

#[unsafe(link_section = ".boot.text")]
unsafe fn value_is_cstr(
    dtb_pa: usize,
    total_size: usize,
    value_off: usize,
    value_len: usize,
    expected: *const u8,
    expected_len: usize,
) -> bool {
    if value_len != expected_len.wrapping_add(1) {
        return false;
    }
    (unsafe { bytes_eq_at(dtb_pa, total_size, value_off, expected, expected_len) })
        && unsafe { byte_at_is_zero(dtb_pa, total_size, value_off.wrapping_add(expected_len)) }
}

#[unsafe(link_section = ".boot.text")]
unsafe fn compatible_contains(
    dtb_pa: usize,
    total_size: usize,
    value_off: usize,
    value_len: usize,
    expected: *const u8,
    expected_len: usize,
) -> bool {
    let mut offset = 0usize;
    while offset < value_len {
        let entry_off = value_off.wrapping_add(offset);
        let mut len = 0usize;
        while can_add_less_than(offset, len, value_len) {
            match unsafe { read_u8_at(dtb_pa, total_size, entry_off.wrapping_add(len)) } {
                Some(0) => break,
                Some(_) => len = len.wrapping_add(1),
                None => return false,
            }
        }
        if len == expected_len
            && unsafe { bytes_eq_at(dtb_pa, total_size, entry_off, expected, expected_len) }
        {
            return true;
        }
        offset = offset.wrapping_add(len).wrapping_add(1);
    }
    false
}

#[unsafe(link_section = ".boot.text")]
unsafe fn cstr_eq(
    dtb_pa: usize,
    total_size: usize,
    offset: usize,
    expected: *const u8,
    expected_len: usize,
) -> bool {
    if !unsafe { bytes_eq_at(dtb_pa, total_size, offset, expected, expected_len) } {
        return false;
    }
    unsafe { byte_at_is_zero(dtb_pa, total_size, offset.wrapping_add(expected_len)) }
}

#[unsafe(link_section = ".boot.text")]
unsafe fn bytes_eq_at(
    dtb_pa: usize,
    total_size: usize,
    offset: usize,
    expected: *const u8,
    expected_len: usize,
) -> bool {
    let mut index = 0usize;
    while index < expected_len {
        let expected_byte = unsafe { boot_load8((expected as usize).wrapping_add(index)) };
        match unsafe { read_u8_at(dtb_pa, total_size, offset.wrapping_add(index)) } {
            Some(byte) if byte == expected_byte => {}
            _ => return false,
        }
        index = index.wrapping_add(1);
    }
    true
}

#[unsafe(link_section = ".boot.text")]
unsafe fn scan_nul(dtb_pa: usize, total_size: usize, offset: usize) -> Option<usize> {
    let mut cursor = offset;
    while cursor < total_size {
        match unsafe { read_u8_at(dtb_pa, total_size, cursor) } {
            Some(0) => return Some(cursor),
            Some(_) => {}
            None => return None,
        }
        cursor = cursor.wrapping_add(1);
    }
    None
}

#[unsafe(link_section = ".boot.text")]
unsafe fn read_be_u32_at(dtb_pa: usize, total_size: usize, offset: usize) -> Option<u32> {
    let end = offset.wrapping_add(4);
    if end < offset || end > total_size {
        return None;
    }
    unsafe { read_be_u32_raw(dtb_pa.wrapping_add(offset)) }
}

#[unsafe(link_section = ".boot.text")]
unsafe fn read_be_u32_raw(addr: usize) -> Option<u32> {
    let b0 = unsafe { boot_load8(addr) };
    let b1 = unsafe { boot_load8(addr.wrapping_add(1)) };
    let b2 = unsafe { boot_load8(addr.wrapping_add(2)) };
    let b3 = unsafe { boot_load8(addr.wrapping_add(3)) };
    Some(((b0 as u32) << 24) | ((b1 as u32) << 16) | ((b2 as u32) << 8) | b3 as u32)
}

#[unsafe(link_section = ".boot.text")]
unsafe fn read_u8_at(dtb_pa: usize, total_size: usize, offset: usize) -> Option<u8> {
    if offset >= total_size {
        return None;
    }
    Some(unsafe { boot_load8(dtb_pa.wrapping_add(offset)) })
}

#[unsafe(link_section = ".boot.text")]
unsafe fn byte_at_is_zero(dtb_pa: usize, total_size: usize, offset: usize) -> bool {
    match unsafe { read_u8_at(dtb_pa, total_size, offset) } {
        Some(byte) => byte == 0,
        None => false,
    }
}

#[unsafe(link_section = ".boot.text")]
const fn align4(value: usize) -> usize {
    value.wrapping_add(3) & !3
}

#[unsafe(link_section = ".boot.text")]
const fn can_add_within(base: usize, add: usize, limit: usize) -> bool {
    let end = base.wrapping_add(add);
    end >= base && end <= limit
}

#[unsafe(link_section = ".boot.text")]
const fn can_add_less_than(base: usize, add: usize, limit: usize) -> bool {
    let end = base.wrapping_add(add);
    end >= base && end < limit
}

#[unsafe(link_section = ".boot.text")]
unsafe fn boot_load8(addr: usize) -> u8 {
    let value: u64;
    unsafe {
        core::arch::asm!(
            "ldrb {value:w}, [{addr}]",
            addr = in(reg) addr,
            value = out(reg) value,
            options(nostack, preserves_flags)
        );
    }
    value as u8
}
