#![no_std]

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MemoryRegion {
    pub start: u64,
    pub size: u64,
    pub kind: MemoryRegionKind,
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum MemoryRegionKind {
    Usable = 0,
    Reserved = 1,
    Mmio = 2,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BootInfo {
    pub boot_cpu_id: u64,
    pub dtb_pa: u64,
    pub rsdp_pa: u64,
    pub memory_map: &'static [MemoryRegion],
}

impl BootInfo {
    pub const fn new() -> Self {
        Self {
            boot_cpu_id: 0,
            dtb_pa: 0,
            rsdp_pa: 0,
            memory_map: &[],
        }
    }
}
