use super::{PhysAddr, VirtAddr};

unsafe extern "C" {
    fn arch_phys_to_virt(pa: usize) -> usize;
    fn arch_virt_to_phys(va: usize) -> usize;
    fn arch_translate_kernel_va(va: usize, out_pa: *mut usize) -> u64;
    fn arch_map_kernel_region(va: usize, pa: usize, size: usize, attr: u32, flags: u64) -> u64;
    fn arch_unmap_kernel_region(va: usize, size: usize) -> u64;
    fn arch_protect_kernel_region(va: usize, size: usize, flags: u64) -> u64;
    fn arch_drop_boot_identity_mapping() -> u64;
    fn arch_switch_to_runtime_kernel_tables() -> u64;
}

#[repr(u32)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VmMemoryAttr {
    DeviceNgRnE = 0,
    NormalWriteBack = 1,
    NormalNonCacheable = 2,
}

#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VmFlags(u64);

impl VmFlags {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const USER: Self = Self(1 << 3);
    pub const GLOBAL: Self = Self(1 << 4);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn union(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VmError {
    NotInitialized,
    InvalidRange,
    NotAligned,
    NotKernelVa,
    AlreadyMapped,
    MissingMapping,
    OutOfFrames,
    Unsupported,
}

#[inline(always)]
pub fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    unsafe { arch_phys_to_virt(pa) }
}

#[inline(always)]
pub fn virt_to_phys_direct(va: VirtAddr) -> Option<PhysAddr> {
    let pa = unsafe { arch_virt_to_phys(va) };
    (pa <= va).then_some(pa)
}

pub fn translate_kernel_va(va: VirtAddr) -> Option<PhysAddr> {
    let mut pa = 0usize;
    match unsafe { arch_translate_kernel_va(va, &mut pa as *mut usize) } {
        0 => Some(pa),
        _ => None,
    }
}

pub unsafe fn map_kernel_region(
    va: VirtAddr,
    pa: PhysAddr,
    size: usize,
    attr: VmMemoryAttr,
    flags: VmFlags,
) -> Result<(), VmError> {
    vm_result(unsafe { arch_map_kernel_region(va, pa, size, attr as u32, flags.bits()) })
}

pub unsafe fn unmap_kernel_region(va: VirtAddr, size: usize) -> Result<(), VmError> {
    vm_result(unsafe { arch_unmap_kernel_region(va, size) })
}

pub unsafe fn protect_kernel_region(
    va: VirtAddr,
    size: usize,
    flags: VmFlags,
) -> Result<(), VmError> {
    vm_result(unsafe { arch_protect_kernel_region(va, size, flags.bits()) })
}

pub unsafe fn drop_boot_identity_mapping() -> Result<(), VmError> {
    vm_result(unsafe { arch_drop_boot_identity_mapping() })
}

pub unsafe fn switch_to_runtime_kernel_tables() -> Result<(), VmError> {
    vm_result(unsafe { arch_switch_to_runtime_kernel_tables() })
}

fn vm_result(code: u64) -> Result<(), VmError> {
    match code {
        0 => Ok(()),
        1 => Err(VmError::NotInitialized),
        2 => Err(VmError::InvalidRange),
        3 => Err(VmError::NotAligned),
        4 => Err(VmError::NotKernelVa),
        5 => Err(VmError::AlreadyMapped),
        6 => Err(VmError::MissingMapping),
        7 => Err(VmError::OutOfFrames),
        _ => Err(VmError::Unsupported),
    }
}
