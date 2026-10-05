//! User memory copy helpers for the current active userspace address space.
//!
//! These helpers are intended for syscall/trap context. They assume the current
//! TTBR0 installed by the scheduler matches `sched::current_user_address_space()`
//! and therefore copy through the current user VA. They do not copy from an
//! arbitrary inactive process address space.
//!
//! The copy loops are bring-up-only: validation happens before the copy, but
//! there is not yet an exception/fixup table for fault recovery if the actual
//! load/store faults.

use alloc::vec::Vec;

pub use crate::config::GENRT_PATH_MAX;

use super::{
    FrameRange, PAGE_SIZE, VirtAddr, align_down,
    vm::{self, StagedUserAddressSpace, UserMapFlags, UserMappingInfo},
};

pub const USER_TEXT_BASE: VirtAddr = 0x0000_0040_0000_0000;
pub const USER_STACK_TOP: VirtAddr = 0x0000_0080_0000_0000;

/// Bring-up copy bound.
///
/// This prevents early syscalls from copying unbounded user buffers before the
/// kernel has fault-recovering copy loops and a richer process memory model.
pub const MAX_USER_COPY: usize = 1024;

const COPY_WORD_BYTES: usize = core::mem::size_of::<usize>();
const _: () = assert!(PAGE_SIZE.is_multiple_of(COPY_WORD_BYTES));

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
        address_space: &StagedUserAddressSpace,
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
        address_space: &StagedUserAddressSpace,
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

/// Copy at most `MAX_USER_COPY` bytes from the current user address space.
///
/// The complete range is validated before copying aligned user words and byte
/// edges. The active user mappings must remain stable throughout the copy;
/// fault recovery is not implemented. This bounded operation allocates
/// nothing, does not block or change IRQ state, and runs in syscall/trap context.
///
/// # Arguments
///
/// * `dst` - Writable kernel buffer receiving the user bytes.
/// * `user_src` - Source virtual address in the active user address space.
///
/// # Returns
///
/// Returns `Ok(())` after copying every byte. An empty destination succeeds
/// without validating `user_src`.
///
/// # Errors
///
/// Returns `UserCopyError::TooLarge` when `dst` exceeds `MAX_USER_COPY`, or a
/// range/permission error. Validation errors leave `dst` unchanged.
pub fn copy_from_user(dst: &mut [u8], user_src: VirtAddr) -> Result<(), UserCopyError> {
    if dst.is_empty() {
        return Ok(());
    }
    validate_user_read_range(user_src, dst.len())?;

    // SAFETY: validation checked the entire readable user range. The current
    // address space remains installed and immutable until the copy completes.
    unsafe { read_validated_user_bytes(dst, user_src) };
    Ok(())
}

/// Copy at most `MAX_USER_COPY` kernel bytes into the current user address space.
///
/// The complete range is validated before copying aligned user words and byte
/// edges. The active user mappings must remain stable throughout the copy;
/// fault recovery is not implemented. This bounded operation allocates
/// nothing, does not block or change IRQ state, and runs in syscall/trap context.
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
/// Validation errors leave the user destination unchanged.
pub fn copy_to_user(user_dst: VirtAddr, src: &[u8]) -> Result<(), UserCopyError> {
    if src.is_empty() {
        return Ok(());
    }
    if src.len() > MAX_USER_COPY {
        return Err(UserCopyError::TooLarge);
    }
    validate_user_range_unbounded(user_dst, src.len(), AccessKind::Write)?;
    // SAFETY: validation checked the entire writable user range, whose
    // immutable mappings remain active until the copy completes.
    unsafe { write_validated_user_bytes(user_dst, src) };
    Ok(())
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
    // SAFETY: the complete value-plus-NUL range was validated as writable.
    unsafe { write_validated_user_bytes(user_dst, value) };
    let nul_dst = user_dst
        .checked_add(value.len())
        .ok_or(UserCopyError::AddressOverflow)?;
    // SAFETY: the complete value-plus-NUL range was validated as writable.
    unsafe { (nul_dst as *mut u8).write_volatile(0) };
    Ok(required)
}

/// # Safety
///
/// `user_src..user_src + dst.len()` must be validated, readable normal user
/// memory with stable active mappings and must not overlap `dst`. There is no
/// fault recovery. Aligned words cannot cross a page boundary because page
/// size is a multiple of `COPY_WORD_BYTES`; byte edges never overread the range.
unsafe fn read_validated_user_bytes(dst: &mut [u8], user_src: VirtAddr) {
    let src = user_src as *const u8;
    let prefix = (user_src.wrapping_neg() & (COPY_WORD_BYTES - 1)).min(dst.len());
    let word_end = dst.len() - (dst.len() - prefix) % COPY_WORD_BYTES;
    let kernel_aligned = dst
        .as_ptr()
        .addr()
        .wrapping_add(prefix)
        .is_multiple_of(COPY_WORD_BYTES);
    let mut offset = 0usize;
    while offset < prefix {
        // SAFETY: offset is inside both buffers; byte access needs no alignment.
        dst[offset] = unsafe { src.add(offset).read_volatile() };
        offset += 1;
    }
    while offset < word_end {
        // SAFETY: the prefix aligned the user pointer and a full word remains
        // inside both buffers. Kernel slice storage need not be word-aligned.
        unsafe {
            let word = src.add(offset).cast::<usize>().read_volatile();
            let kernel_word = dst.as_mut_ptr().add(offset).cast::<usize>();
            if kernel_aligned {
                kernel_word.write(word);
            } else {
                kernel_word.write_unaligned(word);
            }
        }
        offset += COPY_WORD_BYTES;
    }
    while offset < dst.len() {
        // SAFETY: the final byte edges stay inside the validated range.
        dst[offset] = unsafe { src.add(offset).read_volatile() };
        offset += 1;
    }
}

/// # Safety
///
/// `user_dst..user_dst + src.len()` must be validated, writable normal user
/// memory with stable active mappings and must not overlap `src`. There is no
/// fault recovery. Aligned words cannot cross a page boundary; byte edges
/// never overwrite bytes outside the validated range.
unsafe fn write_validated_user_bytes(user_dst: VirtAddr, src: &[u8]) {
    let dst = user_dst as *mut u8;
    let prefix = (user_dst.wrapping_neg() & (COPY_WORD_BYTES - 1)).min(src.len());
    let word_end = src.len() - (src.len() - prefix) % COPY_WORD_BYTES;
    let kernel_aligned = src
        .as_ptr()
        .addr()
        .wrapping_add(prefix)
        .is_multiple_of(COPY_WORD_BYTES);
    let mut offset = 0usize;
    while offset < prefix {
        // SAFETY: offset is inside both buffers; byte access needs no alignment.
        unsafe { dst.add(offset).write_volatile(src[offset]) };
        offset += 1;
    }
    // Select outside the loop so strict-alignment code generation retains
    // aligned kernel loads instead of merging them with bytewise fallback.
    if kernel_aligned {
        while offset < word_end {
            // SAFETY: both pointers are word-aligned and a complete word
            // remains inside the source and validated user destination.
            unsafe {
                let word = src.as_ptr().add(offset).cast::<usize>().read();
                dst.add(offset).cast::<usize>().write_volatile(word);
            }
            offset += COPY_WORD_BYTES;
        }
    } else {
        while offset < word_end {
            // SAFETY: the user pointer is aligned and a complete word remains
            // in both buffers; unaligned kernel storage uses the fallback.
            unsafe {
                let word = src.as_ptr().add(offset).cast::<usize>().read_unaligned();
                dst.add(offset).cast::<usize>().write_volatile(word);
            }
            offset += COPY_WORD_BYTES;
        }
    }
    while offset < src.len() {
        // SAFETY: the final byte edges stay inside the validated range.
        unsafe { dst.add(offset).write_volatile(src[offset]) };
        offset += 1;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise_copy(user_offset: usize, kernel_offset: usize, len: usize) {
        let mut user = alloc::vec![0x9b; len + 2 * COPY_WORD_BYTES];
        let mut kernel = alloc::vec![0xa5; len + 2 * COPY_WORD_BYTES];
        for index in 0..len {
            user[user_offset + index] = index.wrapping_mul(37) as u8;
        }
        let original_user = user.clone();
        // SAFETY: both disjoint host allocations cover the exact copy range.
        unsafe {
            read_validated_user_bytes(
                &mut kernel[kernel_offset..kernel_offset + len],
                user.as_ptr().add(user_offset) as VirtAddr,
            );
        }
        assert_eq!(
            kernel[kernel_offset..kernel_offset + len],
            user[user_offset..user_offset + len]
        );
        assert!(kernel[..kernel_offset].iter().all(|byte| *byte == 0xa5));
        assert!(
            kernel[kernel_offset + len..]
                .iter()
                .all(|byte| *byte == 0xa5)
        );
        assert_eq!(user, original_user);

        user.fill(0x9b);
        let original_kernel = kernel.clone();
        // SAFETY: both disjoint host allocations cover the exact copy range.
        unsafe {
            write_validated_user_bytes(
                user.as_mut_ptr().add(user_offset) as VirtAddr,
                &kernel[kernel_offset..kernel_offset + len],
            );
        }
        assert_eq!(
            user[user_offset..user_offset + len],
            kernel[kernel_offset..kernel_offset + len]
        );
        assert!(user[..user_offset].iter().all(|byte| *byte == 0x9b));
        assert!(user[user_offset + len..].iter().all(|byte| *byte == 0x9b));
        assert_eq!(kernel, original_kernel);
    }

    #[test]
    fn word_copies_preserve_exact_ranges_for_every_alignment_and_byte_tail() {
        for user_offset in 0..COPY_WORD_BYTES {
            for kernel_offset in 0..COPY_WORD_BYTES {
                for len in 0..=3 * COPY_WORD_BYTES {
                    exercise_copy(user_offset, kernel_offset, len);
                }
                exercise_copy(user_offset, kernel_offset, MAX_USER_COPY);
                exercise_copy(user_offset, kernel_offset, GENRT_PATH_MAX + 1);
            }
        }
    }

    #[test]
    fn word_copies_cross_page_boundaries_without_touching_guards() {
        #[repr(align(4096))]
        struct UserPages([u8; 2 * PAGE_SIZE]);

        for edge in 0..COPY_WORD_BYTES {
            let mut user = UserPages([0xa5; 2 * PAGE_SIZE]);
            let start = PAGE_SIZE - COPY_WORD_BYTES + edge;
            let mut kernel = [0u8; 4 * COPY_WORD_BYTES];
            let len = kernel.len();
            for (index, byte) in kernel.iter_mut().enumerate() {
                *byte = index as u8;
            }
            // SAFETY: the disjoint arrays cover a range spanning their two
            // resident user pages and the complete kernel buffer.
            unsafe {
                write_validated_user_bytes(user.0.as_mut_ptr().add(start) as VirtAddr, &kernel);
            }
            assert_eq!(user.0[start..start + len], kernel);
            assert!(user.0[..start].iter().all(|byte| *byte == 0xa5));
            assert!(user.0[start + len..].iter().all(|byte| *byte == 0xa5));
            kernel.fill(0xff);
            // SAFETY: the same resident, disjoint ranges remain valid.
            unsafe {
                read_validated_user_bytes(&mut kernel, user.0.as_ptr().add(start) as VirtAddr);
            }
            for (index, byte) in kernel.iter().enumerate() {
                assert_eq!(*byte, index as u8);
            }
        }
    }

    #[test]
    fn rejected_read_ranges_leave_kernel_buffers_unchanged() {
        let mut buffer = [0xa5; 2 * COPY_WORD_BYTES];
        for (ptr, error) in [
            (0, UserCopyError::NotUserRange),
            (usize::MAX - COPY_WORD_BYTES, UserCopyError::AddressOverflow),
            (
                USER_STACK_TOP - COPY_WORD_BYTES,
                UserCopyError::NotUserRange,
            ),
        ] {
            assert_eq!(copy_from_user(&mut buffer, ptr), Err(error));
            assert_eq!(buffer, [0xa5; 2 * COPY_WORD_BYTES]);
        }
        let mut too_large = [0xa5; MAX_USER_COPY + 1];
        assert_eq!(
            copy_from_user(&mut too_large, USER_TEXT_BASE),
            Err(UserCopyError::TooLarge)
        );
        assert!(too_large.iter().all(|byte| *byte == 0xa5));
        assert_eq!(copy_from_user(&mut [], usize::MAX), Ok(()));
        assert_eq!(copy_to_user(usize::MAX, &[]), Ok(()));
    }
}
