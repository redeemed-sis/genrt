use alloc::vec::Vec;
use core::cell::UnsafeCell;

pub struct RamFile {
    pub path: Vec<u8>,
    pub data: &'static [u8],
}

pub struct MountedRamfs {
    files: Vec<RamFile>,
    dirs: Vec<Vec<u8>>,
}

impl MountedRamfs {
    pub fn new(files: Vec<RamFile>, dirs: Vec<Vec<u8>>) -> Self {
        Self { files, dirs }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MountError {
    AlreadyMounted,
}

struct RamfsCell(UnsafeCell<Option<MountedRamfs>>);

// SAFETY: the initramfs is mounted once during single-core boot before the
// scheduler starts. After a successful mount the filesystem index is immutable;
// task context only performs shared lookups.
unsafe impl Sync for RamfsCell {}

static RAMFS: RamfsCell = RamfsCell(UnsafeCell::new(None));

pub fn mount(fs: MountedRamfs) -> Result<(), MountError> {
    let slot = ramfs_mut();
    if slot.is_some() {
        return Err(MountError::AlreadyMounted);
    }

    *slot = Some(fs);
    Ok(())
}

pub fn is_mounted() -> bool {
    ramfs().is_some()
}

pub fn lookup(path: &[u8]) -> Option<usize> {
    ramfs()?
        .files
        .iter()
        .enumerate()
        .find_map(|(index, file)| (file.path.as_slice() == path).then_some(index))
}

pub fn is_dir(path: &[u8]) -> bool {
    ramfs().is_some_and(|fs| fs.dirs.iter().any(|dir| dir.as_slice() == path))
}

pub fn data(index: usize) -> Option<&'static [u8]> {
    ramfs()?.files.get(index).map(|file| file.data)
}

pub fn data_by_path(path: &[u8]) -> Option<&'static [u8]> {
    data(lookup(path)?)
}

pub fn counts() -> (usize, usize) {
    ramfs()
        .map(|fs| (fs.files.len(), fs.dirs.len()))
        .unwrap_or((0, 0))
}

fn ramfs() -> Option<&'static MountedRamfs> {
    // SAFETY: see `RamfsCell` Sync invariant. Once mounted, the value is not
    // mutated while shared references are used.
    unsafe { (&*RAMFS.0.get()).as_ref() }
}

fn ramfs_mut() -> &'static mut Option<MountedRamfs> {
    // SAFETY: mutation happens only during single-core boot before scheduler
    // start, and mount is rejected after a filesystem is already installed.
    unsafe { &mut *RAMFS.0.get() }
}
