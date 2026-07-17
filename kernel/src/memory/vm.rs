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

/// Copyable identity for an owned TTBR0 address space.
///
/// This value may be stored in scheduler state and used for activation or
/// mapping queries. It cannot destroy page tables; only
/// [`OwnedUserAddressSpace`] owns that capability. Copying this identity does
/// not allocate, block, alter IRQ state, or extend the lifetime of the root.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AddressSpaceId {
    root_pa: PhysAddr,
}

impl AddressSpaceId {
    const fn root_pa(self) -> PhysAddr {
        self.root_pa
    }
}

/// Exclusive owner of allocator-backed TTBR0 page tables.
///
/// Mapping and loading borrow this value; activation and query APIs accept its
/// copyable [`AddressSpaceId`]. The root physical address remains encapsulated.
/// Ownership must be consumed by [`destroy_user_address_space`] after every
/// failed staging path; this type has no implicit `Drop` cleanup.
pub struct OwnedUserAddressSpace {
    id: AddressSpaceId,
}

impl OwnedUserAddressSpace {
    /// Return the non-owning ID used by scheduler and query paths.
    ///
    /// # Returns
    ///
    /// Returns a copyable identifier without transferring ownership. This is
    /// bounded and does not allocate, block, or alter IRQ state.
    pub const fn id(&self) -> AddressSpaceId {
        self.id
    }

    const fn from_root_pa(root_pa: PhysAddr) -> Self {
        Self {
            id: AddressSpaceId { root_pa },
        }
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

/// Create an owned, empty TTBR0 user address space.
///
/// This delegates page-table-root allocation to the architecture VM backend.
/// It may allocate frames and must not run in an IRQ or scheduler fast path.
///
/// # Returns
///
/// Returns the unique root owner. The caller must either transfer it to a
/// process lifecycle path or consume it with [`destroy_user_address_space`].
///
/// # Errors
///
/// Returns backend initialization, frame-allocation, or unsupported-operation
/// errors as [`VmError`].
pub fn create_user_address_space() -> Result<OwnedUserAddressSpace, VmError> {
    let mut root_pa = 0usize;
    match unsafe { arch_create_user_address_space(&mut root_pa as *mut usize) } {
        0 => Ok(OwnedUserAddressSpace::from_root_pa(root_pa)),
        code => Err(vm_error_from_code(code)),
    }
}

/// Destroy an owned TTBR0 root and its page-table frames.
///
/// The operation consumes `aspace`, may return frames to the allocator, and
/// must not run in an IRQ or scheduler fast path.
///
/// # Arguments
///
/// * `aspace` - Unique root owner being permanently destroyed.
///
/// # Returns
///
/// Returns `Ok(())` after the architecture backend released the root.
///
/// # Errors
///
/// Returns backend teardown failures as [`VmError`]. On error, ownership has
/// still been consumed and callers must not reuse the ID as a live root.
///
/// # Safety
///
/// The caller must ensure no active CPU, scheduler thread, mapping operation,
/// or retained architecture context can still reference this address space.
pub unsafe fn destroy_user_address_space(aspace: OwnedUserAddressSpace) -> Result<(), VmError> {
    vm_result(unsafe { arch_destroy_user_address_space(aspace.id.root_pa()) })
}

/// Map one user page into an owned TTBR0 root.
///
/// The architecture backend may allocate intermediate page tables; this path
/// does not block and must stay outside IRQ and scheduler fast paths.
///
/// # Arguments
///
/// * `aspace` - Borrowed owner of the destination TTBR0 root.
/// * `va` - Page-aligned user virtual address.
/// * `pa` - Page-aligned physical frame address.
/// * `flags` - User mapping permissions.
///
/// # Returns
///
/// Returns `Ok(())` after publishing the mapping.
///
/// # Errors
///
/// Returns invalid-range, alignment, existing-mapping, or frame-allocation
/// errors reported by the architecture backend.
///
/// # Safety
///
/// The caller must own the physical frame's lifetime, provide a valid user
/// page range, and ensure that publishing this mapping cannot violate an
/// active address-space or aliasing invariant.
pub unsafe fn map_user_page(
    aspace: &OwnedUserAddressSpace,
    va: VirtAddr,
    pa: PhysAddr,
    flags: UserMapFlags,
) -> Result<(), VmError> {
    vm_result(unsafe { arch_map_user_page(aspace.id.root_pa(), va, pa, flags.bits()) })
}

/// Map a contiguous range of user pages into an owned TTBR0 root.
///
/// This validates alignment and arithmetic before mapping each page. The
/// backend may allocate intermediate page tables; it does not block and must
/// not run in IRQ or scheduler fast paths.
///
/// # Arguments
///
/// * `aspace` - Borrowed owner of the destination TTBR0 root.
/// * `va` - Page-aligned first user virtual address.
/// * `pa` - Page-aligned first physical frame address.
/// * `size` - Page-aligned byte count; zero is a no-op.
/// * `flags` - Permissions applied to every mapped page.
///
/// # Returns
///
/// Returns `Ok(())` after all pages are mapped; a zero `size` succeeds without
/// touching the backend.
///
/// # Errors
///
/// Returns [`VmError::NotAligned`] for unaligned inputs,
/// [`VmError::InvalidRange`] for overflow, or backend mapping errors. Earlier
/// pages remain mapped if a later backend call fails.
pub fn map_user_page_range(
    aspace: &OwnedUserAddressSpace,
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

/// Translate a virtual address through a user address-space identity.
///
/// This is a bounded query that neither allocates nor blocks and may be used
/// with scheduler-held copyable IDs. It does not alter IRQ state.
///
/// # Arguments
///
/// * `aspace` - Copyable identity for the queried TTBR0 root.
/// * `va` - User virtual address to translate.
///
/// # Returns
///
/// Returns `Some` with the physical address when mapped, or `None` when the
/// backend reports no translation.
pub fn translate_user_va(aspace: AddressSpaceId, va: VirtAddr) -> Option<PhysAddr> {
    let mut pa = 0usize;
    match unsafe { arch_translate_user_va(aspace.root_pa(), va, &mut pa as *mut usize) } {
        0 => Some(pa),
        _ => None,
    }
}

/// Query permissions and physical backing for one user virtual address.
///
/// This bounded query neither allocates nor blocks and does not alter IRQ
/// state.
///
/// # Arguments
///
/// * `aspace` - Copyable identity for the queried TTBR0 root.
/// * `va` - User virtual address whose mapping is queried.
///
/// # Returns
///
/// Returns `Some` with mapping metadata, or `None` when no mapping exists.
pub fn query_user_mapping(aspace: AddressSpaceId, va: VirtAddr) -> Option<UserMappingInfo> {
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

/// Activate a user TTBR0 root for the current execution context.
///
/// This is bounded and allocation-free. Scheduler handoff calls it with local
/// IRQ exclusion; it must not block.
///
/// # Arguments
///
/// * `aspace` - Live root identity selected for the current user thread.
///
/// # Returns
///
/// Returns `Ok(())` after the architecture installed the root.
///
/// # Errors
///
/// Returns backend activation failures as [`VmError`].
///
/// # Safety
///
/// The caller must ensure `aspace` names a live owner and that replacing the
/// current TTBR0 is valid for the active saved/live context.
pub unsafe fn activate_user_address_space(aspace: AddressSpaceId) -> Result<(), VmError> {
    vm_result(unsafe { arch_activate_user_address_space(aspace.root_pa()) })
}

/// Clear the current user TTBR0 root for kernel-thread execution.
///
/// This is bounded and allocation-free. Scheduler handoff calls it with local
/// IRQ exclusion; it must not block.
///
/// # Returns
///
/// Returns `Ok(())` after the architecture cleared the root.
///
/// # Errors
///
/// Returns backend clearing failures as [`VmError`].
///
/// # Safety
///
/// The caller must ensure the active context will not resume userspace until a
/// valid user address space is activated again.
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
