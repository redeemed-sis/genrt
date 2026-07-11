use alloc::vec::Vec;

use cpio_reader::Mode;

use crate::memory::vm;

use super::ramfs::{self, MountedRamfs, RamFile};

const NEWC_MAGIC: &[u8] = b"070701";
const TRAILER_NAME: &[u8] = b"TRAILER!!!";
const MODE_TYPE_MASK: u32 = 0o170_000;
const MODE_DIRECTORY: u32 = 0o040_000;
const MODE_REGULAR_FILE: u32 = 0o100_000;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InitramfsError {
    Parse,
    UnsupportedFormat,
    InvalidPath,
    DuplicatePath,
    UnsupportedFileType,
    MissingInit,
    OutOfMemory,
    AlreadyMounted,
}

pub fn mount_from_loader_region() -> Result<(), InitramfsError> {
    let range = vm::initramfs_load_range();
    let size = range.end - range.start;
    let image_va = vm::phys_to_virt(range.start);
    crate::info!(
        "initramfs: mounting image at pa=0x{:x} max_size={}",
        range.start,
        size
    );

    // SAFETY: the platform reserves `initramfs_load_range()` for the QEMU
    // loader payload. The region is permanently excluded from the frame
    // allocator because file data slices borrow directly from this image.
    let image = unsafe { core::slice::from_raw_parts(image_va as *const u8, size) };
    mount_from_cpio_newc(image)
}

pub fn mount_from_cpio_newc(image: &'static [u8]) -> Result<(), InitramfsError> {
    validate_newc_image(image)?;

    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut entries = 0usize;

    for entry in cpio_reader::iter_files(image) {
        entries += 1;
        if entry.nlink() > 1 {
            return Err(InitramfsError::UnsupportedFileType);
        }

        let path = canonical_archive_path(entry.name())?;
        if path_exists(&files, &dirs, &path) {
            return Err(InitramfsError::DuplicatePath);
        }

        match file_type(entry.mode()) {
            MODE_REGULAR_FILE => {
                files
                    .try_reserve_exact(1)
                    .map_err(|_| InitramfsError::OutOfMemory)?;
                files.push(RamFile {
                    path,
                    data: entry.file(),
                });
            }
            MODE_DIRECTORY => {
                dirs.try_reserve_exact(1)
                    .map_err(|_| InitramfsError::OutOfMemory)?;
                dirs.push(path);
            }
            _ => return Err(InitramfsError::UnsupportedFileType),
        }
    }

    if entries == 0 {
        return Err(InitramfsError::Parse);
    }
    if !files.iter().any(|file| file.path.as_slice() == b"/init") {
        return Err(InitramfsError::MissingInit);
    }

    let fs = MountedRamfs::new(files, dirs).map_err(mount_index_error)?;
    ramfs::mount(fs).map_err(mount_index_error)?;
    let (file_count, dir_count) = ramfs::counts();
    crate::info!("initramfs: mounted {file_count} files, {dir_count} directories");
    Ok(())
}

pub fn init_file() -> Result<&'static [u8], InitramfsError> {
    ramfs::data_by_path(b"/init").ok_or(InitramfsError::MissingInit)
}

fn validate_newc_image(image: &[u8]) -> Result<(), InitramfsError> {
    if !image.starts_with(NEWC_MAGIC) {
        return Err(InitramfsError::UnsupportedFormat);
    }
    if !contains_subslice(image, TRAILER_NAME) {
        return Err(InitramfsError::Parse);
    }
    Ok(())
}

fn canonical_archive_path(name: &str) -> Result<Vec<u8>, InitramfsError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes[0] == b'/' {
        return Err(InitramfsError::InvalidPath);
    }

    let mut out = Vec::new();
    out.try_reserve_exact(bytes.len() + 1)
        .map_err(|_| InitramfsError::OutOfMemory)?;
    out.push(b'/');

    let mut component_start = 0usize;
    let mut cursor = 0usize;
    while cursor <= bytes.len() {
        if cursor == bytes.len() || bytes[cursor] == b'/' {
            let component = &bytes[component_start..cursor];
            if component.is_empty() || component == b"." || component == b".." {
                return Err(InitramfsError::InvalidPath);
            }
            out.extend_from_slice(component);
            if cursor != bytes.len() {
                out.push(b'/');
            }
            component_start = cursor + 1;
        }
        cursor += 1;
    }

    Ok(out)
}

fn path_exists(files: &[RamFile], dirs: &[Vec<u8>], path: &[u8]) -> bool {
    files.iter().any(|file| file.path.as_slice() == path)
        || dirs.iter().any(|dir| dir.as_slice() == path)
}

fn file_type(mode: Mode) -> u32 {
    mode.bits() & MODE_TYPE_MASK
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn mount_index_error(err: ramfs::MountError) -> InitramfsError {
    match err {
        ramfs::MountError::AlreadyMounted => InitramfsError::AlreadyMounted,
        ramfs::MountError::DuplicatePath => InitramfsError::DuplicatePath,
        ramfs::MountError::InvalidPath => InitramfsError::InvalidPath,
        ramfs::MountError::OutOfMemory => InitramfsError::OutOfMemory,
    }
}
