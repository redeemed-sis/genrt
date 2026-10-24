use alloc::{sync::Arc, vec::Vec};

use crate::sync::OncePublication;

/// File entry discovered in the mounted readonly ramfs.
///
/// `data` borrows from the permanently reserved initramfs image; it is not
/// copied into heap-owned storage.
pub struct RamFile {
    pub path: Vec<u8>,
    pub data: &'static [u8],
}

struct RamDir {
    path: Vec<u8>,
    entries: Vec<RamDirEntry>,
}

/// Kind of a directory entry exposed through `getdents64`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DirEntryKind {
    /// Regular readonly ramfs file.
    File,
    /// Directory in the mounted ramfs index.
    Directory,
}

/// Borrowed directory entry metadata from the mounted ramfs index.
///
/// The name is one immediate child component, never a full path and never
/// containing `/`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DirEntryRef<'a> {
    pub name: &'a [u8],
    pub kind: DirEntryKind,
    pub ino: u64,
}

#[derive(Clone)]
struct RamDirEntry {
    name: Vec<u8>,
    kind: DirEntryKind,
    ino: u64,
}

/// Immutable mounted ramfs index.
///
/// Construction synthesizes parent directories from file paths, rejects
/// file/directory conflicts, and sorts each directory's immediate children for
/// deterministic iteration.
pub struct MountedRamfs {
    files: Vec<RamFile>,
    dirs: Vec<RamDir>,
}

impl MountedRamfs {
    /// Build a readonly ramfs index from initramfs files and explicit directory
    /// entries.
    ///
    /// The resulting index always contains `/`. Parent directories are
    /// synthesized for every file and explicit directory path, so archives do
    /// not need to carry separate entries for `/bin` or `/etc`.
    ///
    /// # Arguments
    ///
    /// * `files` - Regular files with absolute normalized ramfs paths and data
    ///   borrowed from the reserved initramfs image.
    /// * `explicit_dirs` - Absolute normalized directory paths found in the
    ///   archive. Missing parent directories are synthesized.
    ///
    /// # Returns
    ///
    /// Returns a mounted-index value with sorted files, synthesized
    /// directories, and lexicographically sorted immediate-child directory
    /// entries.
    ///
    /// # Errors
    ///
    /// Returns `MountError::DuplicatePath` for duplicate files, duplicate
    /// sibling entries, or file/directory conflicts; `MountError::InvalidPath`
    /// for malformed paths; and `MountError::OutOfMemory` if boot-time index
    /// allocation fails.
    pub fn new(
        mut files: Vec<RamFile>,
        mut explicit_dirs: Vec<Vec<u8>>,
    ) -> Result<Self, MountError> {
        files.sort_by(|lhs, rhs| lhs.path.cmp(&rhs.path));
        reject_duplicate_files(&files)?;

        for dir in &explicit_dirs {
            validate_absolute_path(dir)?;
        }
        explicit_dirs.sort();
        reject_duplicate_paths(&explicit_dirs)?;

        let mut dir_paths = Vec::new();
        push_dir_path(&mut dir_paths, b"/")?;
        for dir in explicit_dirs {
            push_dir_with_parents(&mut dir_paths, &dir)?;
        }
        for file in &files {
            push_parent_dirs(&mut dir_paths, &file.path)?;
        }

        // Explicit paths and synthesized parents intentionally overlap. Input
        // duplicates were rejected above; this deduplication only merges the
        // two sources into one directory index.
        dir_paths.sort();
        dir_paths.dedup();
        reject_file_dir_conflicts(&files, &dir_paths)?;

        let mut dirs = Vec::new();
        dirs.try_reserve_exact(dir_paths.len())
            .map_err(|_| MountError::OutOfMemory)?;
        for path in dir_paths {
            dirs.push(RamDir {
                path,
                entries: Vec::new(),
            });
        }

        let mut fs = Self { files, dirs };
        fs.populate_dir_entries()?;
        Ok(fs)
    }

    fn populate_dir_entries(&mut self) -> Result<(), MountError> {
        let file_count = self.files.len();
        for file_index in 0..file_count {
            let (parent_index, name) = {
                let path = &self.files[file_index].path;
                let parent = parent_path(path)?;
                let parent_index = self.lookup_dir(parent).ok_or(MountError::InvalidPath)?;
                (parent_index, try_copy_bytes(basename(path)?)?)
            };
            self.append_dir_entry(parent_index, name, DirEntryKind::File, file_ino(file_index))?;
        }

        let dir_count = self.dirs.len();
        for dir_index in 0..dir_count {
            if self.dirs[dir_index].path.as_slice() == b"/" {
                continue;
            }
            let (parent_index, name) = {
                let path = &self.dirs[dir_index].path;
                let parent = parent_path(path)?;
                let parent_index = self.lookup_dir(parent).ok_or(MountError::InvalidPath)?;
                (parent_index, try_copy_bytes(basename(path)?)?)
            };
            self.append_dir_entry(
                parent_index,
                name,
                DirEntryKind::Directory,
                dir_ino(dir_index),
            )?;
        }

        for dir in &mut self.dirs {
            dir.entries.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));
            reject_duplicate_dir_entries(&dir.entries)?;
        }
        Ok(())
    }

    fn append_dir_entry(
        &mut self,
        parent_index: usize,
        name: Vec<u8>,
        kind: DirEntryKind,
        ino: u64,
    ) -> Result<(), MountError> {
        let entries = &mut self.dirs[parent_index].entries;
        entries
            .try_reserve_exact(1)
            .map_err(|_| MountError::OutOfMemory)?;
        entries.push(RamDirEntry { name, kind, ino });
        Ok(())
    }

    fn lookup_file(&self, path: &[u8]) -> Option<usize> {
        self.files
            .iter()
            .enumerate()
            .find_map(|(index, file)| (file.path.as_slice() == path).then_some(index))
    }

    fn lookup_dir(&self, path: &[u8]) -> Option<usize> {
        self.dirs
            .iter()
            .enumerate()
            .find_map(|(index, dir)| (dir.path.as_slice() == path).then_some(index))
    }
}

/// Error while mounting or indexing the ramfs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MountError {
    /// A ramfs is already mounted.
    AlreadyMounted,
    /// A file, directory, or sibling directory entry appears more than once.
    DuplicatePath,
    /// A path is not an absolute normalized ramfs path.
    InvalidPath,
    /// Heap allocation failed while building the boot-time ramfs index.
    OutOfMemory,
}

static RAMFS: OncePublication<Arc<MountedRamfs>> = OncePublication::new();

/// Install the global readonly ramfs index.
///
/// This allocates the instance's reference-counted owner once during boot,
/// before the scheduler starts, and publishes its immutable index.
///
/// # Arguments
///
/// * `fs` - Fully built readonly ramfs index to install globally.
///
/// # Returns
///
/// Returns `Ok(())` after storing the index.
///
/// # Errors
///
/// Returns `MountError::AlreadyMounted` if an index has already been installed.
pub fn mount(fs: MountedRamfs) -> Result<(), MountError> {
    RAMFS
        .publish(Arc::new(fs))
        .map_err(|_| MountError::AlreadyMounted)
}

/// Return whether a ramfs index has already been mounted.
///
/// # Returns
///
/// Returns `true` once `mount()` has succeeded, otherwise `false`.
pub fn is_mounted() -> bool {
    ramfs().is_some()
}

/// Look up a regular file path and return its stable ramfs file index.
///
/// # Arguments
///
/// * `path` - Absolute normalized ramfs path, for example `b"/hello.txt"`.
///
/// # Returns
///
/// Returns `Some(file_index)` when `path` names a regular file, or `None` if
/// ramfs is not mounted or the path is absent/not a regular file.
pub fn lookup_file(path: &[u8]) -> Option<usize> {
    ramfs()?.lookup_file(path)
}

/// Look up a directory path and return its stable ramfs directory index.
///
/// # Arguments
///
/// * `path` - Absolute normalized ramfs path, for example `b"/bin"`.
///
/// # Returns
///
/// Returns `Some(dir_index)` when `path` names a directory, or `None` if ramfs
/// is not mounted or the path is absent/not a directory.
pub fn lookup_dir(path: &[u8]) -> Option<usize> {
    ramfs()?.lookup_dir(path)
}

/// Return the stable directory index for the mounted ramfs root.
///
/// # Returns
///
/// Returns `Some(index)` for canonical path `/`, or `None` before ramfs mount.
pub fn root_dir_index() -> Option<usize> {
    lookup_dir(b"/")
}

/// Return the canonical absolute path for a stable ramfs directory index.
///
/// Directory indexes remain private to the immutable archive adapter. Generic
/// cwd and descriptors use mount-aware VFS references instead.
///
/// # Arguments
///
/// * `dir_index` - Directory index returned by `lookup_dir()`.
///
/// # Returns
///
/// Returns the directory's canonical absolute path, or `None` before mount or
/// for an invalid index.
pub fn dir_path(dir_index: usize) -> Option<&'static [u8]> {
    ramfs()?.dirs.get(dir_index).map(|dir| dir.path.as_slice())
}

/// Return whether `path` names a mounted ramfs directory.
///
/// # Arguments
///
/// * `path` - Absolute normalized ramfs path.
///
/// # Returns
///
/// Returns `true` when `path` resolves to a directory in the mounted ramfs.
pub fn is_dir(path: &[u8]) -> bool {
    lookup_dir(path).is_some()
}

/// Return file contents by ramfs file index.
///
/// # Arguments
///
/// * `index` - File index previously returned by `lookup_file()`.
///
/// # Returns
///
/// Returns `Some(data)` for a valid file index, or `None` if ramfs is not
/// mounted or the index is stale/out of bounds.
pub fn data(index: usize) -> Option<&'static [u8]> {
    ramfs()?.files.get(index).map(|file| file.data)
}

/// Look up a regular file by path and return its contents.
///
/// # Arguments
///
/// * `path` - Absolute normalized ramfs path.
///
/// # Returns
///
/// Returns `Some(data)` when `path` names a regular file, or `None` otherwise.
pub fn data_by_path(path: &[u8]) -> Option<&'static [u8]> {
    data(lookup_file(path)?)
}

/// Return the number of immediate children in a directory.
///
/// # Arguments
///
/// * `dir_index` - Directory index previously returned by `lookup_dir()`.
///
/// # Returns
///
/// Returns `Some(count)` for a valid directory index, or `None` if ramfs is not
/// mounted or the index is stale/out of bounds.
pub fn dir_entry_count(dir_index: usize) -> Option<usize> {
    ramfs()?.dirs.get(dir_index).map(|dir| dir.entries.len())
}

/// Return the `offset`-th immediate child of a directory.
///
/// Offsets are directory-entry indexes and are the unit stored in directory file
/// descriptors. The returned entry borrows immutable mount-time metadata.
///
/// # Arguments
///
/// * `dir_index` - Directory index previously returned by `lookup_dir()`.
/// * `offset` - Zero-based immediate-child index within that directory.
///
/// # Returns
///
/// Returns `Some(entry)` when the directory and offset are valid. Returns
/// `None` at end-of-directory, when ramfs is not mounted, or when `dir_index` is
/// invalid.
pub fn dir_entry_at(dir_index: usize, offset: usize) -> Option<DirEntryRef<'static>> {
    let entry = ramfs()?.dirs.get(dir_index)?.entries.get(offset)?;
    Some(DirEntryRef {
        name: entry.name.as_slice(),
        kind: entry.kind,
        ino: entry.ino,
    })
}

/// Return `(regular_file_count, directory_count)` for diagnostics.
///
/// # Returns
///
/// Returns the mounted ramfs file and directory counts. Returns `(0, 0)` before
/// ramfs is mounted.
pub fn counts() -> (usize, usize) {
    ramfs()
        .map(|fs| (fs.files.len(), fs.dirs.len()))
        .unwrap_or((0, 0))
}

fn ramfs() -> Option<&'static MountedRamfs> {
    RAMFS.get().map(Arc::as_ref)
}

/// Clone the root backend instance after initramfs indexing.
/// # Returns
/// Pinned instance or none before mount; no allocation or blocking.
pub(crate) fn root_backend() -> Option<Arc<dyn super::backend::Backend>> {
    RAMFS
        .get()
        .map(|fs| fs.clone() as Arc<dyn super::backend::Backend>)
}

impl super::backend::Backend for MountedRamfs {
    fn root(&self) -> super::backend::Node {
        super::backend::Node {
            id: dir_ino(self.lookup_dir(b"/").unwrap()),
            kind: super::backend::Kind::Directory,
        }
    }

    fn poll(
        &self,
        request: &mut super::backend::Request,
        _wake: Option<crate::sched::WaitToken>,
    ) -> core::task::Poll<Result<super::backend::Reply, crate::errno::Errno>> {
        use super::backend::{DirEntry, Kind, Node, Operation, Reply, Stat};
        use crate::errno;
        let node = request.node;
        let Some(index) = node_index(node) else {
            return core::task::Poll::Ready(Err(errno::ENOENT));
        };
        let size = if node.kind == Kind::File {
            self.files.get(index).map(|f| f.data.len() as u64)
        } else {
            self.dirs.get(index).map(|_| 0)
        };
        let result = (|| {
            let size = size.ok_or(errno::ENOENT)?;
            match &request.operation {
                Operation::Lookup(name) => {
                    if node.kind != Kind::Directory {
                        return Err(errno::ENOTDIR);
                    }
                    let directory = &self.dirs[index];
                    let mut path = directory.path.clone();
                    if path != b"/" {
                        path.push(b'/');
                    }
                    path.extend_from_slice(name);
                    if let Some(i) = self.lookup_dir(&path) {
                        Ok(Reply::Node(Node {
                            id: dir_ino(i),
                            kind: Kind::Directory,
                        }))
                    } else if let Some(i) = self.lookup_file(&path) {
                        Ok(Reply::Node(Node {
                            id: file_ino(i),
                            kind: Kind::File,
                        }))
                    } else {
                        Err(errno::ENOENT)
                    }
                }
                Operation::Open(flags) => {
                    if flags.write || flags.create || flags.truncate || flags.append {
                        Err(errno::EROFS)
                    } else {
                        Ok(Reply::Done)
                    }
                }
                Operation::Read { offset, limit } => {
                    if node.kind == Kind::Directory {
                        return Err(errno::EISDIR);
                    }
                    let data = self.files[index].data;
                    let start = usize::try_from(*offset)
                        .unwrap_or(usize::MAX)
                        .min(data.len());
                    let count = (*limit).min(request.buffer.len()).min(data.len() - start);
                    request.buffer[..count].copy_from_slice(&data[start..start + count]);
                    Ok(Reply::Io {
                        count,
                        next: offset.checked_add(count as u64).ok_or(errno::EOVERFLOW)?,
                    })
                }
                Operation::Readdir { offset } => {
                    if node.kind != Kind::Directory {
                        return Err(errno::ENOTDIR);
                    }
                    let entry = usize::try_from(*offset)
                        .ok()
                        .and_then(|i| self.dirs[index].entries.get(i));
                    let entry = entry.map(|entry| {
                        let mut path = self.dirs[index].path.clone();
                        if path != b"/" {
                            path.push(b'/');
                        }
                        path.extend_from_slice(&entry.name);
                        let node = match entry.kind {
                            DirEntryKind::File => Node {
                                id: file_ino(self.lookup_file(&path).unwrap()),
                                kind: Kind::File,
                            },
                            DirEntryKind::Directory => Node {
                                id: dir_ino(self.lookup_dir(&path).unwrap()),
                                kind: Kind::Directory,
                            },
                        };
                        DirEntry {
                            name: entry.name.clone(),
                            node,
                        }
                    });
                    Ok(Reply::Entry(entry))
                }
                Operation::Create(_) | Operation::Truncate { .. } | Operation::Write { .. } => {
                    Err(errno::EROFS)
                }
                Operation::Seek {
                    current,
                    displacement,
                    whence,
                } => {
                    let base = match whence {
                        0 => 0,
                        1 => *current,
                        2 => size,
                        _ => return Err(errno::EINVAL),
                    };
                    let next = base
                        .checked_add_signed(*displacement)
                        .ok_or(errno::EINVAL)?;
                    Ok(Reply::Position(next))
                }
                Operation::Stat => Ok(Reply::Stat(Stat { node, size })),
                Operation::Sync => Ok(Reply::Done),
            }
        })();
        core::task::Poll::Ready(result)
    }

    fn close(
        &self,
        _node: super::backend::Node,
        _state: Option<alloc::boxed::Box<dyn core::any::Any + Send>>,
    ) {
    }

    fn executable(&self, node: super::backend::Node) -> Option<&'static [u8]> {
        if node.kind != super::backend::Kind::File {
            return None;
        }
        self.files.get(node_index(node)?).map(|f| f.data)
    }
}

fn push_dir_with_parents(dirs: &mut Vec<Vec<u8>>, path: &[u8]) -> Result<(), MountError> {
    validate_absolute_path(path)?;
    push_parent_dirs(dirs, path)?;
    push_dir_path(dirs, path)
}

fn push_parent_dirs(dirs: &mut Vec<Vec<u8>>, path: &[u8]) -> Result<(), MountError> {
    validate_absolute_path(path)?;
    push_dir_path(dirs, b"/")?;

    let mut cursor = 1usize;
    while cursor < path.len() {
        if path[cursor] == b'/' {
            push_dir_path(dirs, &path[..cursor])?;
        }
        cursor += 1;
    }
    Ok(())
}

fn push_dir_path(dirs: &mut Vec<Vec<u8>>, path: &[u8]) -> Result<(), MountError> {
    dirs.try_reserve_exact(1)
        .map_err(|_| MountError::OutOfMemory)?;
    dirs.push(try_copy_bytes(path)?);
    Ok(())
}

fn try_copy_bytes(bytes: &[u8]) -> Result<Vec<u8>, MountError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|_| MountError::OutOfMemory)?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn reject_duplicate_files(files: &[RamFile]) -> Result<(), MountError> {
    for pair in files.windows(2) {
        if pair[0].path == pair[1].path {
            return Err(MountError::DuplicatePath);
        }
    }
    Ok(())
}

fn reject_duplicate_paths(paths: &[Vec<u8>]) -> Result<(), MountError> {
    for pair in paths.windows(2) {
        if pair[0] == pair[1] {
            return Err(MountError::DuplicatePath);
        }
    }
    Ok(())
}

fn reject_file_dir_conflicts(files: &[RamFile], dirs: &[Vec<u8>]) -> Result<(), MountError> {
    for file in files {
        if dirs
            .iter()
            .any(|dir| dir.as_slice() == file.path.as_slice())
        {
            return Err(MountError::DuplicatePath);
        }
    }
    Ok(())
}

fn reject_duplicate_dir_entries(entries: &[RamDirEntry]) -> Result<(), MountError> {
    for pair in entries.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(MountError::DuplicatePath);
        }
    }
    Ok(())
}

fn parent_path(path: &[u8]) -> Result<&[u8], MountError> {
    validate_absolute_path(path)?;
    let Some(pos) = path.iter().rposition(|byte| *byte == b'/') else {
        return Err(MountError::InvalidPath);
    };
    if pos == 0 { Ok(b"/") } else { Ok(&path[..pos]) }
}

fn basename(path: &[u8]) -> Result<&[u8], MountError> {
    validate_absolute_path(path)?;
    let Some(pos) = path.iter().rposition(|byte| *byte == b'/') else {
        return Err(MountError::InvalidPath);
    };
    let name = &path[pos + 1..];
    if name.is_empty() {
        Err(MountError::InvalidPath)
    } else {
        Ok(name)
    }
}

fn validate_absolute_path(path: &[u8]) -> Result<(), MountError> {
    if path.is_empty()
        || path[0] != b'/'
        || path.len() > crate::config::GENRT_PATH_MAX
        || path.contains(&0)
        || (path.len() > 1
            && path[1..].split(|b| *b == b'/').any(|component| {
                component.is_empty()
                    || component.len() > super::backend::NAME_MAX
                    || matches!(component, b"." | b"..")
            }))
    {
        return Err(MountError::InvalidPath);
    }
    Ok(())
}

fn file_ino(index: usize) -> u64 {
    1 + index as u64
}

fn dir_ino(index: usize) -> u64 {
    0x8000_0000 + index as u64
}

fn node_index(node: super::backend::Node) -> Option<usize> {
    let base = match node.kind {
        super::backend::Kind::File => file_ino(0),
        super::backend::Kind::Directory => dir_ino(0),
    };
    usize::try_from(node.id.checked_sub(base)?).ok()
}
