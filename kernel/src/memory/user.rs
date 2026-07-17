//! User memory copy helpers for the current active userspace address space.
//!
//! These helpers are intended for syscall/trap context. They assume the current
//! TTBR0 installed by the scheduler matches `sched::current_user_address_space()`
//! and therefore copy through the current user VA. They do not copy from an
//! arbitrary inactive process address space.
//!
//! The byte loops are bring-up-only: validation happens before the copy, but
//! there is not yet an exception/fixup table for fault recovery if the actual
//! load/store faults.

use alloc::vec::Vec;

pub use crate::limits::GENRT_PATH_MAX;

use super::{
    FrameRange, PAGE_SIZE, VirtAddr, align_down,
    vm::{self, OwnedUserAddressSpace, UserMapFlags, UserMappingInfo},
};

pub const USER_TEXT_BASE: VirtAddr = 0x0000_0040_0000_0000;
pub const USER_STACK_TOP: VirtAddr = 0x0000_0080_0000_0000;

/// Bring-up copy bound.
///
/// This prevents early syscalls from copying unbounded user buffers before the
/// kernel has fault-recovering copy loops and a richer process memory model.
pub const MAX_USER_COPY: usize = 1024;

/// Exclusive ownership of mapped user-stack frames.
///
/// The stack can be moved into a schedulable thread but is never copied. Its
/// destructor releases frames after the owner has been removed from scheduler
/// state; callers must therefore extract it before any IRQ-disabled lifecycle
/// transition that will reclaim a thread.
pub(crate) struct OwnedUserStack {
    frames: FrameRange,
    base: VirtAddr,
    size: usize,
}

impl OwnedUserStack {
    /// Allocate, zero, and map one non-executable user stack.
    ///
    /// This operation allocates physical frames and mutates a borrowed TTBR0
    /// owner. It may not run in IRQ or scheduler fast paths.
    ///
    /// # Arguments
    ///
    /// * `address_space` - Owner of the TTBR0 root receiving the mapping.
    /// * `size` - Page-aligned stack size ending at [`USER_STACK_TOP`].
    ///
    /// # Returns
    ///
    /// Returns the uniquely owned mapped stack.
    ///
    /// # Errors
    ///
    /// Returns [`vm::VmError::OutOfFrames`] when frame allocation fails and
    /// propagates invalid-range, alignment, or mapping errors.
    pub(crate) fn allocate(
        address_space: &OwnedUserAddressSpace,
        size: usize,
    ) -> Result<Self, vm::VmError> {
        if size == 0 || size & (PAGE_SIZE - 1) != 0 || size > USER_STACK_TOP {
            return Err(vm::VmError::InvalidRange);
        }
        let frames =
            super::alloc_contiguous_frames(size / PAGE_SIZE).ok_or(vm::VmError::OutOfFrames)?;
        super::zero_phys_range(frames);
        let base = USER_STACK_TOP - size;
        if let Err(error) =
            vm::map_user_page_range(address_space, base, frames.start, size, UserMapFlags::WRITE)
        {
            super::free_contiguous_frames(frames);
            return Err(error);
        }
        Ok(Self { frames, base, size })
    }

    /// Return the backing frames for bounded kernel-side initialization.
    ///
    /// # Returns
    ///
    /// Returns a copy of the owned frame range without transferring ownership.
    /// Reading it is bounded and does not allocate, block, or alter IRQ state.
    pub(crate) const fn frames(&self) -> FrameRange {
        self.frames
    }

    /// Return the mapped user virtual base.
    ///
    /// # Returns
    ///
    /// Returns the lowest mapped user address without transferring ownership.
    /// Reading it is bounded and does not allocate, block, or alter IRQ state.
    pub(crate) const fn base(&self) -> VirtAddr {
        self.base
    }

    /// Clone this mapped stack into another owned user address space.
    ///
    /// # Arguments
    ///
    /// * `address_space` - Newly created address-space owner receiving the
    ///   cloned writable stack mapping.
    ///
    /// # Returns
    ///
    /// Returns a new stack with copied frames and the same user virtual range.
    ///
    /// # Errors
    ///
    /// Returns frame-allocation or mapping errors. On error, every partially
    /// allocated frame is released before returning. This operation may
    /// allocate and must not run in an IRQ or scheduler fast path.
    pub(crate) fn clone_into(
        &self,
        address_space: &OwnedUserAddressSpace,
    ) -> Result<Self, vm::VmError> {
        let frames = super::clone_frame_range(self.frames).map_err(|error| match error {
            super::FrameRangeCloneError::InvalidRange => vm::VmError::InvalidRange,
            super::FrameRangeCloneError::OutOfFrames => vm::VmError::OutOfFrames,
        })?;
        if let Err(error) = vm::map_user_page_range(
            address_space,
            self.base,
            frames.start,
            self.size,
            UserMapFlags::WRITE,
        ) {
            super::free_contiguous_frames(frames);
            return Err(error);
        }
        Ok(Self {
            frames,
            base: self.base,
            size: self.size,
        })
    }
}

impl Drop for OwnedUserStack {
    fn drop(&mut self) {
        if self.frames.start != 0 {
            super::free_contiguous_frames(self.frames);
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UserCopyError {
    Empty,
    TooLarge,
    NameTooLong,
    AddressOverflow,
    NotUserRange,
    NotMapped,
    NotReadable,
    NotWritable,
    NoCurrentAddressSpace,
    OutOfMemory,
}

pub fn copy_from_user(dst: &mut [u8], user_src: VirtAddr) -> Result<(), UserCopyError> {
    if dst.is_empty() {
        return Ok(());
    }
    validate_user_read_range(user_src, dst.len())?;

    let mut offset = 0usize;
    while offset < dst.len() {
        // SAFETY: validation checked the full range against the current TTBR0
        // mapping and read permissions. This is bring-up-only: it assumes user
        // mappings remain stable during the syscall and does not yet provide
        // fault recovery if the actual load faults.
        dst[offset] = unsafe { (user_src as *const u8).add(offset).read_volatile() };
        offset += 1;
    }
    Ok(())
}

/// Copy at most `MAX_USER_COPY` kernel bytes into the current user address space.
///
/// # Arguments
///
/// * `user_dst` - Destination virtual address in the active user address space.
/// * `src` - Kernel bytes to copy.
///
/// # Returns
///
/// Returns `Ok(())` after copying every byte. An empty source succeeds without
/// validating `user_dst`.
///
/// # Errors
///
/// Returns `UserCopyError::TooLarge` when `src` exceeds `MAX_USER_COPY`, or the
/// permission/range errors returned while validating the active user mapping.
pub fn copy_to_user(user_dst: VirtAddr, src: &[u8]) -> Result<(), UserCopyError> {
    if src.is_empty() {
        return Ok(());
    }
    if src.len() > MAX_USER_COPY {
        return Err(UserCopyError::TooLarge);
    }
    validate_user_range_unbounded(user_dst, src.len(), AccessKind::Write)?;
    write_validated_user_bytes(user_dst, src)
}

/// Copy a bounded kernel byte string and terminating NUL into userspace.
///
/// The complete destination range is validated before either part is written,
/// and no temporary pathname-sized kernel buffer is allocated.
///
/// # Arguments
///
/// * `user_dst` - Destination virtual address in the active user address space.
/// * `value` - Kernel string bytes without a terminating NUL.
/// * `max_len` - Maximum total byte count including the terminating NUL.
///
/// # Returns
///
/// Returns the number of bytes copied, including the terminating NUL.
///
/// # Errors
///
/// Returns `UserCopyError::TooLarge` when `value` plus NUL exceeds `max_len`,
/// `UserCopyError::AddressOverflow` when the size cannot be represented, or the
/// permission/range errors returned while validating the active user mapping.
pub fn copy_cstr_to_user(
    user_dst: VirtAddr,
    value: &[u8],
    max_len: usize,
) -> Result<usize, UserCopyError> {
    let required = value
        .len()
        .checked_add(1)
        .ok_or(UserCopyError::AddressOverflow)?;
    if required > max_len {
        return Err(UserCopyError::TooLarge);
    }
    validate_user_range_unbounded(user_dst, required, AccessKind::Write)?;
    write_validated_user_bytes(user_dst, value)?;
    let nul_dst = user_dst
        .checked_add(value.len())
        .ok_or(UserCopyError::AddressOverflow)?;
    // SAFETY: the complete value-plus-NUL range was validated as writable.
    unsafe { (nul_dst as *mut u8).write_volatile(0) };
    Ok(required)
}

fn write_validated_user_bytes(user_dst: VirtAddr, src: &[u8]) -> Result<(), UserCopyError> {
    let mut offset = 0usize;
    while offset < src.len() {
        let chunk_start = user_dst
            .checked_add(offset)
            .ok_or(UserCopyError::AddressOverflow)?;
        let page_remaining = PAGE_SIZE - (chunk_start & (PAGE_SIZE - 1));
        let chunk_len = page_remaining.min(src.len() - offset);
        let mut chunk_offset = 0usize;
        while chunk_offset < chunk_len {
            // SAFETY: validation checked the full range against the current
            // TTBR0 mapping and write permissions. This is bring-up-only and
            // does not yet recover from a faulting store.
            unsafe {
                (chunk_start as *mut u8)
                    .add(chunk_offset)
                    .write_volatile(src[offset + chunk_offset])
            };
            chunk_offset += 1;
        }
        offset += chunk_len;
    }
    Ok(())
}

pub fn validate_user_read_range(ptr: VirtAddr, len: usize) -> Result<(), UserCopyError> {
    validate_user_range(ptr, len, AccessKind::Read)
}

pub fn validate_user_write_range(ptr: VirtAddr, len: usize) -> Result<(), UserCopyError> {
    validate_user_range(ptr, len, AccessKind::Write)
}

/// Copy one bounded non-empty pathname C string from the current userspace.
///
/// # Arguments
///
/// * `path_ptr` - Userspace address of the NUL-terminated pathname.
///
/// # Returns
///
/// Returns owned pathname bytes without the terminating NUL.
///
/// # Errors
///
/// Returns `UserCopyError::Empty` for an empty pathname and otherwise
/// propagates bounded C-string copy errors from `copy_cstr_from_user()`.
pub fn copy_path_cstr_from_user(path_ptr: VirtAddr) -> Result<Vec<u8>, UserCopyError> {
    let path = copy_cstr_from_user(path_ptr, GENRT_PATH_MAX)?;
    if path.is_empty() {
        return Err(UserCopyError::Empty);
    }
    Ok(path)
}

/// Copy a bounded NUL-terminated string from the current userspace.
///
/// Allocation grows in small fallible chunks instead of reserving `max_len`
/// immediately. User pages are validated once per scanned page chunk.
///
/// # Arguments
///
/// * `ptr` - Userspace address of the first string byte.
/// * `max_len` - Maximum returned bytes excluding the terminating NUL.
///
/// # Returns
///
/// Returns owned bytes before the first NUL.
///
/// # Errors
///
/// Returns `UserCopyError::NameTooLong` when no NUL appears within the bound,
/// `UserCopyError::OutOfMemory` if allocation fails, or a user mapping/range
/// validation error for inaccessible input.
pub fn copy_cstr_from_user(ptr: VirtAddr, max_len: usize) -> Result<Vec<u8>, UserCopyError> {
    let mut path = Vec::new();

    let mut cursor = ptr;
    let mut scanned = 0usize;
    while scanned <= max_len {
        let page_end = align_down(cursor, PAGE_SIZE)
            .checked_add(PAGE_SIZE)
            .ok_or(UserCopyError::AddressOverflow)?;
        let remaining_scan = (max_len + 1) - scanned;
        let chunk_len = page_end.saturating_sub(cursor).min(remaining_scan);
        validate_user_read_range_unbounded(cursor, chunk_len)?;

        let mut offset = 0usize;
        while offset < chunk_len {
            // SAFETY: the current chunk is validated as user-readable. This is
            // still bring-up-only and does not recover from a faulting load.
            let byte = unsafe { (cursor as *const u8).add(offset).read_volatile() };
            if byte == 0 {
                return Ok(path);
            }
            if path.len() == max_len {
                return Err(UserCopyError::NameTooLong);
            }
            if path.len() == path.capacity() {
                let additional = (max_len - path.len()).min(64);
                path.try_reserve_exact(additional)
                    .map_err(|_| UserCopyError::OutOfMemory)?;
            }
            path.push(byte);
            offset += 1;
        }

        cursor = cursor
            .checked_add(chunk_len)
            .ok_or(UserCopyError::AddressOverflow)?;
        scanned += chunk_len;
    }

    Err(UserCopyError::NameTooLong)
}

#[derive(Copy, Clone)]
enum AccessKind {
    Read,
    Write,
}

fn validate_user_range(ptr: VirtAddr, len: usize, access: AccessKind) -> Result<(), UserCopyError> {
    if len == 0 {
        return Err(UserCopyError::Empty);
    }
    if len > MAX_USER_COPY {
        return Err(UserCopyError::TooLarge);
    }
    validate_user_range_unbounded(ptr, len, access)
}

fn validate_user_read_range_unbounded(ptr: VirtAddr, len: usize) -> Result<(), UserCopyError> {
    validate_user_range_unbounded(ptr, len, AccessKind::Read)
}

fn validate_user_range_unbounded(
    ptr: VirtAddr,
    len: usize,
    access: AccessKind,
) -> Result<(), UserCopyError> {
    if len == 0 {
        return Err(UserCopyError::Empty);
    }
    let end = ptr.checked_add(len).ok_or(UserCopyError::AddressOverflow)?;
    if ptr < USER_TEXT_BASE || end > USER_STACK_TOP || ptr >= end {
        return Err(UserCopyError::NotUserRange);
    }

    let address_space =
        crate::sched::current_user_address_space().ok_or(UserCopyError::NoCurrentAddressSpace)?;

    let mut cursor = align_down(ptr, PAGE_SIZE);
    while cursor < end {
        let mapping =
            vm::query_user_mapping(address_space, cursor).ok_or(UserCopyError::NotMapped)?;
        check_mapping(mapping, access)?;
        cursor = cursor
            .checked_add(PAGE_SIZE)
            .ok_or(UserCopyError::AddressOverflow)?;
    }
    Ok(())
}

fn check_mapping(mapping: UserMappingInfo, access: AccessKind) -> Result<(), UserCopyError> {
    if !mapping.user {
        return Err(UserCopyError::NotUserRange);
    }

    match access {
        AccessKind::Read if !mapping.readable => Err(UserCopyError::NotReadable),
        AccessKind::Write if !mapping.writable => Err(UserCopyError::NotWritable),
        _ => Ok(()),
    }
}
