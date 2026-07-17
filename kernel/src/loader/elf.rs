//! Minimal ELF64 userspace loader.
//!
//! The loader owns only executable image setup. Process code owns stack setup,
//! process table state, and final thread creation. This module accepts a small
//! static AArch64 `ET_EXEC` subset and maps `PT_LOAD` segments into a supplied
//! TTBR0 address space.

use alloc::vec::Vec;

use elf::{
    ElfBytes,
    abi::{ET_EXEC, PF_R, PF_W, PF_X, PT_DYNAMIC, PT_INTERP, PT_LOAD, PT_TLS, SHT_REL, SHT_RELA},
    endian::AnyEndian,
    file::Class,
    segment::ProgramHeader,
};

use crate::{
    loader::elf_arch,
    memory::{
        self, FrameRange, PAGE_SIZE, VirtAddr,
        user::{USER_STACK_TOP, USER_TEXT_BASE},
        vm::{self, OwnedUserAddressSpace, UserMapFlags, VmError},
    },
};

#[derive(Debug, Eq, PartialEq)]
pub struct UserElfImage {
    pub entry: VirtAddr,
    segments: Vec<UserElfSegment>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UserElfSegment {
    pub va: VirtAddr,
    pub size: usize,
    pub frames: FrameRange,
    pub flags: UserMapFlags,
}

impl UserElfImage {
    fn empty(entry: VirtAddr) -> Self {
        Self {
            entry,
            segments: Vec::new(),
        }
    }

    pub fn from_segments(entry: VirtAddr, segments: Vec<UserElfSegment>) -> Self {
        Self { entry, segments }
    }

    pub fn segments(&self) -> &[UserElfSegment] {
        &self.segments
    }

    fn reserve_segments(&mut self, capacity: usize) -> Result<(), ElfLoadError> {
        self.segments
            .try_reserve_exact(capacity)
            .map_err(|_| ElfLoadError::FrameAllocationFailed)
    }

    fn push_segment(&mut self, segment: UserElfSegment) {
        self.segments.push(segment);
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ElfLoadError {
    Parse,
    UnsupportedClass,
    UnsupportedEndian,
    UnsupportedMachine,
    UnsupportedType,
    UnsupportedProgramHeader,
    DynamicLinkingUnsupported,
    InvalidProgramHeader,
    SegmentOutOfBounds,
    SegmentAddressOverflow,
    SegmentNotPageAligned,
    InvalidUserRange,
    FrameAllocationFailed,
    MappingFailed,
}

pub fn load_user_elf(
    image: &[u8],
    address_space: &mut OwnedUserAddressSpace,
) -> Result<UserElfImage, ElfLoadError> {
    let file = ElfBytes::<AnyEndian>::minimal_parse(image).map_err(|_| ElfLoadError::Parse)?;
    validate_header(&file)?;
    reject_relocations(&file)?;
    let entry = u64_to_usize(file.ehdr.e_entry)?;
    validate_user_range(entry, 1)?;

    crate::debug!(
        "elf: parsing image size={} entry=0x{:x}",
        image.len(),
        entry
    );

    let mut loaded = UserElfImage::empty(entry);
    let result = load_segments(image, &file, address_space, &mut loaded);
    if result.is_err() {
        free_loaded_segments(&loaded);
    }
    result.map(|()| loaded)
}

pub fn free_loaded_segments(image: &UserElfImage) {
    for segment in image.segments() {
        if segment.frames.start != 0 {
            memory::free_contiguous_frames(segment.frames);
        }
    }
}

fn load_segments(
    image: &[u8],
    file: &ElfBytes<'_, AnyEndian>,
    address_space: &OwnedUserAddressSpace,
    loaded: &mut UserElfImage,
) -> Result<(), ElfLoadError> {
    let segments = file.segments().ok_or(ElfLoadError::InvalidProgramHeader)?;
    if segments.is_empty() {
        return Err(ElfLoadError::InvalidProgramHeader);
    }
    loaded.reserve_segments(segments.len())?;

    for ph in segments {
        match ph.p_type {
            PT_LOAD => load_segment(image, address_space, ph, loaded)?,
            PT_INTERP | PT_DYNAMIC | PT_TLS => {
                return Err(ElfLoadError::DynamicLinkingUnsupported);
            }
            _ => {}
        }
    }

    if loaded.segments().is_empty() {
        return Err(ElfLoadError::InvalidProgramHeader);
    }
    Ok(())
}

fn load_segment(
    image: &[u8],
    address_space: &OwnedUserAddressSpace,
    ph: ProgramHeader,
    loaded: &mut UserElfImage,
) -> Result<(), ElfLoadError> {
    let offset = u64_to_usize(ph.p_offset)?;
    let vaddr = u64_to_usize(ph.p_vaddr)?;
    let filesz = u64_to_usize(ph.p_filesz)?;
    let memsz = u64_to_usize(ph.p_memsz)?;
    let align = u64_to_usize(ph.p_align)?;

    if memsz == 0 {
        return Ok(());
    }
    if filesz > memsz {
        return Err(ElfLoadError::InvalidProgramHeader);
    }
    validate_program_flags(ph.p_flags)?;
    validate_load_alignment(vaddr, offset, align)?;

    let file_end = offset
        .checked_add(filesz)
        .ok_or(ElfLoadError::SegmentOutOfBounds)?;
    if file_end > image.len() {
        return Err(ElfLoadError::SegmentOutOfBounds);
    }

    let seg_end = vaddr
        .checked_add(memsz)
        .ok_or(ElfLoadError::SegmentAddressOverflow)?;
    validate_user_range(vaddr, memsz)?;

    let map_start = memory::align_down(vaddr, PAGE_SIZE);
    let map_end =
        memory::align_up(seg_end, PAGE_SIZE).ok_or(ElfLoadError::SegmentAddressOverflow)?;
    validate_user_range(map_start, map_end - map_start)?;
    let map_size = map_end
        .checked_sub(map_start)
        .ok_or(ElfLoadError::SegmentAddressOverflow)?;
    let frames = map_size / PAGE_SIZE;
    let frame_range =
        memory::alloc_contiguous_frames(frames).ok_or(ElfLoadError::FrameAllocationFailed)?;

    memory::zero_phys_range(frame_range);
    memory::copy_bytes_to_phys(
        frame_range.start + (vaddr - map_start),
        &image[offset..file_end],
    );
    let flags = map_flags_from_program_flags(ph.p_flags);
    if let Err(err) = vm::map_user_page_range(
        &address_space,
        map_start,
        frame_range.start,
        map_size,
        flags,
    ) {
        memory::free_contiguous_frames(frame_range);
        return Err(map_vm_error(err));
    }
    loaded.push_segment(UserElfSegment {
        va: map_start,
        size: map_size,
        frames: frame_range,
        flags,
    });

    crate::debug!(
        "elf: PT_LOAD vaddr=0x{:x} filesz={} memsz={} flags=0x{:x} mapped_pa={:?}",
        vaddr,
        filesz,
        memsz,
        ph.p_flags,
        frame_range
    );
    Ok(())
}

fn validate_header(file: &ElfBytes<'_, AnyEndian>) -> Result<(), ElfLoadError> {
    if file.ehdr.class != Class::ELF64 {
        return Err(ElfLoadError::UnsupportedClass);
    }
    if file.ehdr.endianness != AnyEndian::Little {
        return Err(ElfLoadError::UnsupportedEndian);
    }
    if file.ehdr.e_type != ET_EXEC {
        return Err(ElfLoadError::UnsupportedType);
    }
    elf_arch::validate_machine(file.ehdr.e_machine)
}

fn reject_relocations(file: &ElfBytes<'_, AnyEndian>) -> Result<(), ElfLoadError> {
    let Some(sections) = file.section_headers() else {
        return Ok(());
    };

    for section in sections {
        if matches!(section.sh_type, SHT_REL | SHT_RELA) {
            return Err(ElfLoadError::UnsupportedProgramHeader);
        }
    }
    Ok(())
}

fn validate_program_flags(flags: u32) -> Result<(), ElfLoadError> {
    let readable = flags & PF_R != 0;
    let writable = flags & PF_W != 0;
    let executable = flags & PF_X != 0;

    if !readable || (writable && executable) {
        return Err(ElfLoadError::UnsupportedProgramHeader);
    }
    Ok(())
}

fn validate_load_alignment(
    vaddr: VirtAddr,
    offset: usize,
    align: usize,
) -> Result<(), ElfLoadError> {
    if align <= 1 {
        return Ok(());
    }
    if vaddr % align != offset % align {
        return Err(ElfLoadError::SegmentNotPageAligned);
    }
    Ok(())
}

fn validate_user_range(start: VirtAddr, size: usize) -> Result<(), ElfLoadError> {
    let end = start
        .checked_add(size)
        .ok_or(ElfLoadError::SegmentAddressOverflow)?;
    if start < USER_TEXT_BASE || start >= end || end > USER_STACK_TOP {
        return Err(ElfLoadError::InvalidUserRange);
    }
    Ok(())
}

fn map_flags_from_program_flags(flags: u32) -> UserMapFlags {
    let mut out = UserMapFlags::empty();
    if flags & PF_W != 0 {
        out = out.union(UserMapFlags::WRITE);
    }
    if flags & PF_X != 0 {
        out = out.union(UserMapFlags::EXECUTE);
    }
    out
}

fn u64_to_usize(value: u64) -> Result<usize, ElfLoadError> {
    usize::try_from(value).map_err(|_| ElfLoadError::SegmentAddressOverflow)
}

fn map_vm_error(err: VmError) -> ElfLoadError {
    match err {
        VmError::OutOfFrames => ElfLoadError::FrameAllocationFailed,
        _ => ElfLoadError::MappingFailed,
    }
}
