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

use super::{
    PAGE_SIZE, VirtAddr,
    vm::{self, UserMappingInfo},
};

pub const USER_TEXT_BASE: VirtAddr = 0x0000_0040_0000_0000;
pub const USER_STACK_TOP: VirtAddr = 0x0000_0080_0000_0000;

/// Bring-up copy bound.
///
/// This prevents early syscalls from copying unbounded user buffers before the
/// kernel has fault-recovering copy loops and a richer process memory model.
pub const MAX_USER_COPY: usize = 1024;

/// POSIX-like pathname bound, excluding the terminating NUL byte.
///
/// `open()` receives NUL-terminated userspace pathnames, but the kernel must
/// bound userspace scanning for RT predictability and memory safety. Paths
/// longer than `GENRT_PATH_MAX` fail with `UserCopyError::NameTooLong`.
pub const GENRT_PATH_MAX: usize = 4096;

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

pub fn copy_to_user(user_dst: VirtAddr, src: &[u8]) -> Result<(), UserCopyError> {
    if src.is_empty() {
        return Ok(());
    }
    validate_user_write_range(user_dst, src.len())?;

    let mut offset = 0usize;
    while offset < src.len() {
        // SAFETY: validation checked the full range against the current TTBR0
        // mapping and write permissions. This is bring-up-only: it assumes user
        // mappings remain stable during the syscall and does not yet provide
        // fault recovery if the actual store faults.
        unsafe {
            (user_dst as *mut u8)
                .add(offset)
                .write_volatile(src[offset])
        };
        offset += 1;
    }
    Ok(())
}

pub fn validate_user_read_range(ptr: VirtAddr, len: usize) -> Result<(), UserCopyError> {
    validate_user_range(ptr, len, AccessKind::Read)
}

pub fn validate_user_write_range(ptr: VirtAddr, len: usize) -> Result<(), UserCopyError> {
    validate_user_range(ptr, len, AccessKind::Write)
}

pub fn copy_path_cstr_from_user(path_ptr: VirtAddr) -> Result<Vec<u8>, UserCopyError> {
    let path = copy_cstr_from_user(path_ptr, GENRT_PATH_MAX)?;
    if path.is_empty() {
        return Err(UserCopyError::Empty);
    }
    Ok(path)
}

pub fn copy_cstr_from_user(ptr: VirtAddr, max_len: usize) -> Result<Vec<u8>, UserCopyError> {
    let mut path = Vec::new();
    path.try_reserve_exact(max_len)
        .map_err(|_| UserCopyError::OutOfMemory)?;

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

const fn align_down(value: usize, align: usize) -> usize {
    (value / align) * align
}
