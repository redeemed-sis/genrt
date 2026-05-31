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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UserCopyError {
    Empty,
    TooLarge,
    AddressOverflow,
    NotUserRange,
    NotMapped,
    NotReadable,
    NotWritable,
    NoCurrentAddressSpace,
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
