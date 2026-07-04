use super::{PAGE_SIZE, PhysAddr, PhysRange, VirtAddr};

unsafe extern "C" {
    fn arch_phys_to_virt(pa: usize) -> usize;
    fn arch_virt_to_phys(va: usize) -> usize;
    fn arch_initramfs_load_pa() -> usize;
    fn arch_initramfs_reserved_size() -> usize;
    fn arch_translate_kernel_va(va: usize, out_pa: *mut usize) -> u64;
    fn arch_map_kernel_region(va: usize, pa: usize, size: usize, attr: u32, flags: u64) -> u64;
    fn arch_unmap_kernel_region(va: usize, size: usize) -> u64;
    fn arch_protect_kernel_region(va: usize, size: usize, flags: u64) -> u64;
    fn arch_drop_boot_identity_mapping() -> u64;
    fn arch_switch_to_runtime_kernel_tables() -> u64;
    fn arch_create_user_address_space(out_root_pa: *mut usize) -> u64;
    fn arch_destroy_user_address_space(root_pa: usize) -> u64;
    fn arch_map_user_page(root_pa: usize, va: usize, pa: usize, flags: u64) -> u64;
    fn arch_translate_user_va(root_pa: usize, va: usize, out_pa: *mut usize) -> u64;
    fn arch_query_user_mapping(root_pa: usize, va: usize, out_info: *mut UserMappingInfo) -> u64;
    fn arch_activate_user_address_space(root_pa: usize) -> u64;
    fn arch_clear_user_address_space() -> u64;
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

#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UserMapFlags(u64);

impl UserMapFlags {
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);

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

#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UserAddressSpace {
    root_pa: PhysAddr,
}

impl UserAddressSpace {
    pub const fn root_pa(self) -> PhysAddr {
        self.root_pa
    }

    const fn from_root_pa(root_pa: PhysAddr) -> Self {
        Self { root_pa }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UserMappingInfo {
    pub pa: PhysAddr,
    pub user: bool,
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
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

pub fn initramfs_load_range() -> PhysRange {
    let start = unsafe { arch_initramfs_load_pa() };
    let size = unsafe { arch_initramfs_reserved_size() };
    PhysRange {
        start,
        end: start.saturating_add(size),
    }
}

pub fn create_user_address_space() -> Result<UserAddressSpace, VmError> {
    let mut root_pa = 0usize;
    match unsafe { arch_create_user_address_space(&mut root_pa as *mut usize) } {
        0 => Ok(UserAddressSpace::from_root_pa(root_pa)),
        code => Err(vm_error_from_code(code)),
    }
}

pub unsafe fn destroy_user_address_space(aspace: UserAddressSpace) -> Result<(), VmError> {
    vm_result(unsafe { arch_destroy_user_address_space(aspace.root_pa()) })
}

pub unsafe fn map_user_page(
    aspace: UserAddressSpace,
    va: VirtAddr,
    pa: PhysAddr,
    flags: UserMapFlags,
) -> Result<(), VmError> {
    vm_result(unsafe { arch_map_user_page(aspace.root_pa(), va, pa, flags.bits()) })
}

pub fn map_user_page_range(
    aspace: UserAddressSpace,
    va: VirtAddr,
    pa: PhysAddr,
    size: usize,
    flags: UserMapFlags,
) -> Result<(), VmError> {
    if size == 0 {
        return Ok(());
    }
    if va & (PAGE_SIZE - 1) != 0 || pa & (PAGE_SIZE - 1) != 0 || size & (PAGE_SIZE - 1) != 0 {
        return Err(VmError::NotAligned);
    }
    if va.checked_add(size).is_none() || pa.checked_add(size).is_none() {
        return Err(VmError::InvalidRange);
    }

    let mut offset = 0usize;
    while offset < size {
        // SAFETY: this wrapper validates page alignment and bounded arithmetic.
        // The caller owns the target TTBR0 root for the lifetime of the mapping.
        unsafe {
            map_user_page(aspace, va + offset, pa + offset, flags)?;
        }
        offset += PAGE_SIZE;
    }
    Ok(())
}

pub fn translate_user_va(aspace: UserAddressSpace, va: VirtAddr) -> Option<PhysAddr> {
    let mut pa = 0usize;
    match unsafe { arch_translate_user_va(aspace.root_pa(), va, &mut pa as *mut usize) } {
        0 => Some(pa),
        _ => None,
    }
}

pub fn query_user_mapping(aspace: UserAddressSpace, va: VirtAddr) -> Option<UserMappingInfo> {
    let mut info = UserMappingInfo {
        pa: 0,
        user: false,
        readable: false,
        writable: false,
        executable: false,
    };
    match unsafe { arch_query_user_mapping(aspace.root_pa(), va, &mut info as *mut _) } {
        0 => Some(info),
        _ => None,
    }
}

pub unsafe fn activate_user_address_space(aspace: UserAddressSpace) -> Result<(), VmError> {
    vm_result(unsafe { arch_activate_user_address_space(aspace.root_pa()) })
}

pub unsafe fn clear_user_address_space() -> Result<(), VmError> {
    vm_result(unsafe { arch_clear_user_address_space() })
}

fn vm_result(code: u64) -> Result<(), VmError> {
    if code == 0 {
        Ok(())
    } else {
        Err(vm_error_from_code(code))
    }
}

fn vm_error_from_code(code: u64) -> VmError {
    match code {
        0 => VmError::Unsupported,
        1 => VmError::NotInitialized,
        2 => VmError::InvalidRange,
        3 => VmError::NotAligned,
        4 => VmError::NotKernelVa,
        5 => VmError::AlreadyMapped,
        6 => VmError::MissingMapping,
        7 => VmError::OutOfFrames,
        _ => VmError::Unsupported,
    }
}
