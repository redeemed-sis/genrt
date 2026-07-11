use super::ramfs;

pub const MAX_FDS: usize = 32;
pub const STDIN: usize = 0;
pub const STDOUT: usize = 1;
pub const STDERR: usize = 2;
const FIRST_USER_FD: usize = 3;

/// Errors returned by the bounded per-process FD table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FdError {
    /// The descriptor is out of range, closed, or reserved stdio is not usable
    /// for the requested operation.
    BadFd,
    /// A regular-file read operation was attempted on a directory handle.
    IsDirectory,
    /// Directory iteration was attempted on a regular-file handle.
    NotDirectory,
    /// No user descriptor slot is available.
    TooManyOpenFiles,
    /// The descriptor exists, but the requested operation is not implemented.
    Unsupported,
}

/// Open file description stored directly in a process FD table.
///
/// genrt currently has no shared open-file-description layer, so `fork()` copies
/// this value and parent/child offsets advance independently.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileHandle {
    /// Readonly ramfs regular file with byte offset.
    RamFile {
        file_index: usize,
        offset: usize,
        readable: bool,
    },
    /// Readonly ramfs directory with entry-index offset.
    RamDir { dir_index: usize, offset: usize },
}

/// Directory entry returned together with its absolute directory offset.
///
/// The syscall layer uses `offset + 1` as the Linux-like `d_off` value for the
/// next record.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FdDirEntry {
    pub offset: usize,
    pub entry: ramfs::DirEntryRef<'static>,
}

/// Fixed-size per-process file descriptor table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FdTable {
    entries: [Option<FileHandle>; MAX_FDS],
}

impl FdTable {
    /// Create an empty FD table with stdio descriptors reserved.
    ///
    /// # Returns
    ///
    /// Returns a table with all user-openable slots empty. Descriptors `0`,
    /// `1`, and `2` are intentionally reserved outside the table's normal open
    /// path.
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_FDS],
        }
    }

    /// Close every non-stdio descriptor.
    ///
    /// Handles currently own only offsets into immutable ramfs metadata, so
    /// cleanup is just clearing slots.
    ///
    /// # Returns
    ///
    /// This function does not return a result; all open user descriptor slots
    /// are cleared unconditionally.
    pub fn close_all(&mut self) {
        for entry in &mut self.entries {
            *entry = None;
        }
    }

    /// Open a readonly ramfs regular file and return the lowest free fd.
    ///
    /// # Arguments
    ///
    /// * `file_index` - Ramfs file index previously returned by
    ///   `ramfs::lookup_file()`.
    ///
    /// # Returns
    ///
    /// Returns the lowest available user descriptor number, starting at `3`.
    ///
    /// # Errors
    ///
    /// Returns `FdError::TooManyOpenFiles` if every user descriptor slot is
    /// already occupied.
    pub fn open_ram_file(&mut self, file_index: usize) -> Result<usize, FdError> {
        let Some(fd) = self.entries[FIRST_USER_FD..]
            .iter()
            .position(Option::is_none)
            .map(|offset| offset + FIRST_USER_FD)
        else {
            return Err(FdError::TooManyOpenFiles);
        };

        self.entries[fd] = Some(FileHandle::RamFile {
            file_index,
            offset: 0,
            readable: true,
        });
        Ok(fd)
    }

    /// Open a readonly ramfs directory and return the lowest free fd.
    ///
    /// # Arguments
    ///
    /// * `dir_index` - Ramfs directory index previously returned by
    ///   `ramfs::lookup_dir()`.
    ///
    /// # Returns
    ///
    /// Returns the lowest available user descriptor number, starting at `3`.
    ///
    /// # Errors
    ///
    /// Returns `FdError::TooManyOpenFiles` if every user descriptor slot is
    /// already occupied.
    pub fn open_ram_dir(&mut self, dir_index: usize) -> Result<usize, FdError> {
        let Some(fd) = self.entries[FIRST_USER_FD..]
            .iter()
            .position(Option::is_none)
            .map(|offset| offset + FIRST_USER_FD)
        else {
            return Err(FdError::TooManyOpenFiles);
        };

        self.entries[fd] = Some(FileHandle::RamDir {
            dir_index,
            offset: 0,
        });
        Ok(fd)
    }

    /// Close a user descriptor.
    ///
    /// Stdio descriptors `0..=2` are reserved and are not closeable here.
    ///
    /// # Arguments
    ///
    /// * `fd` - User descriptor number to close.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` after clearing an open user descriptor.
    ///
    /// # Errors
    ///
    /// Returns `FdError::BadFd` if `fd` is reserved, out of range, or already
    /// closed.
    pub fn close(&mut self, fd: usize) -> Result<(), FdError> {
        let entry = self.user_entry_mut(fd)?;
        if entry.is_none() {
            return Err(FdError::BadFd);
        }

        *entry = None;
        Ok(())
    }

    /// Return whether a user descriptor slot is currently open.
    ///
    /// # Arguments
    ///
    /// * `fd` - User descriptor number to test.
    ///
    /// # Returns
    ///
    /// Returns `true` only for open user descriptors. Reserved stdio
    /// descriptors and out-of-range values return `false`.
    pub fn is_open(&self, fd: usize) -> bool {
        self.user_entry(fd).is_some_and(Option::is_some)
    }

    /// Return the readable slice at the current regular-file offset.
    ///
    /// The offset is advanced separately by `advance_read()` only after the
    /// syscall layer successfully copies the bytes to userspace.
    ///
    /// # Arguments
    ///
    /// * `fd` - User descriptor to read.
    /// * `max_len` - Maximum number of bytes to expose from the current file
    ///   offset.
    ///
    /// # Returns
    ///
    /// Returns a borrowed slice of at most `max_len` bytes. The slice can be
    /// empty at EOF.
    ///
    /// # Errors
    ///
    /// Returns `FdError::BadFd` for invalid, closed, or non-readable file
    /// descriptors, and `FdError::IsDirectory` if `fd` is a directory handle.
    pub fn read_slice(&self, fd: usize, max_len: usize) -> Result<&'static [u8], FdError> {
        match self.user_entry(fd).and_then(|entry| *entry) {
            Some(FileHandle::RamFile {
                file_index,
                offset,
                readable,
            }) if readable => {
                let data = ramfs::data(file_index).ok_or(FdError::BadFd)?;
                let start = offset.min(data.len());
                let end = start.saturating_add(max_len).min(data.len());
                Ok(&data[start..end])
            }
            Some(FileHandle::RamFile { .. }) => Err(FdError::BadFd),
            Some(FileHandle::RamDir { .. }) => Err(FdError::IsDirectory),
            None => Err(FdError::BadFd),
        }
    }

    /// Advance a regular-file offset after a successful read copy.
    ///
    /// # Arguments
    ///
    /// * `fd` - User descriptor whose regular-file offset should advance.
    /// * `amount` - Number of bytes successfully returned to userspace.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` after clamping the new offset to the file length.
    ///
    /// # Errors
    ///
    /// Returns `FdError::BadFd` for invalid, closed, or non-readable file
    /// descriptors, and `FdError::IsDirectory` if `fd` is a directory handle.
    pub fn advance_read(&mut self, fd: usize, amount: usize) -> Result<(), FdError> {
        match self.user_entry_mut(fd)?.as_mut() {
            Some(FileHandle::RamFile {
                file_index,
                offset,
                readable,
            }) if *readable => {
                let data_len = ramfs::data(*file_index).ok_or(FdError::BadFd)?.len();
                *offset = offset.saturating_add(amount).min(data_len);
                Ok(())
            }
            Some(FileHandle::RamFile { .. }) => Err(FdError::BadFd),
            Some(FileHandle::RamDir { .. }) => Err(FdError::IsDirectory),
            None => Err(FdError::BadFd),
        }
    }

    /// Return a directory entry relative to the descriptor's current offset.
    ///
    /// This does not mutate the descriptor. `getdents64` can therefore encode
    /// entries into a kernel buffer and only commit the offset after
    /// `copy_to_user()` succeeds.
    ///
    /// # Arguments
    ///
    /// * `fd` - User descriptor that must refer to a directory.
    /// * `relative_offset` - Entry offset relative to the descriptor's current
    ///   directory offset.
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(entry))` when an entry exists, `Ok(None)` at
    /// end-of-directory, and does not modify the descriptor offset.
    ///
    /// # Errors
    ///
    /// Returns `FdError::BadFd` for invalid or closed descriptors and
    /// `FdError::NotDirectory` if `fd` is a regular file handle.
    pub fn dir_entry_at(
        &self,
        fd: usize,
        relative_offset: usize,
    ) -> Result<Option<FdDirEntry>, FdError> {
        match self.user_entry(fd).and_then(|entry| *entry) {
            Some(FileHandle::RamDir { dir_index, offset }) => {
                let entry_offset = offset.saturating_add(relative_offset);
                Ok(
                    ramfs::dir_entry_at(dir_index, entry_offset).map(|entry| FdDirEntry {
                        offset: entry_offset,
                        entry,
                    }),
                )
            }
            Some(FileHandle::RamFile { .. }) => Err(FdError::NotDirectory),
            None => Err(FdError::BadFd),
        }
    }

    /// Advance a directory offset after successfully returning entries.
    ///
    /// # Arguments
    ///
    /// * `fd` - User descriptor that must refer to a directory.
    /// * `amount` - Number of directory entries successfully returned to
    ///   userspace.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` after clamping the new offset to the directory entry
    /// count.
    ///
    /// # Errors
    ///
    /// Returns `FdError::BadFd` for invalid or closed descriptors and
    /// `FdError::NotDirectory` if `fd` is a regular file handle.
    pub fn advance_dir_read(&mut self, fd: usize, amount: usize) -> Result<(), FdError> {
        match self.user_entry_mut(fd)?.as_mut() {
            Some(FileHandle::RamDir { dir_index, offset }) => {
                let entry_count = ramfs::dir_entry_count(*dir_index).ok_or(FdError::BadFd)?;
                *offset = offset.saturating_add(amount).min(entry_count);
                Ok(())
            }
            Some(FileHandle::RamFile { .. }) => Err(FdError::NotDirectory),
            None => Err(FdError::BadFd),
        }
    }

    fn user_entry(&self, fd: usize) -> Option<&Option<FileHandle>> {
        (fd >= FIRST_USER_FD)
            .then_some(fd)
            .and_then(|fd| self.entries.get(fd))
    }

    fn user_entry_mut(&mut self, fd: usize) -> Result<&mut Option<FileHandle>, FdError> {
        if fd < FIRST_USER_FD {
            return Err(FdError::BadFd);
        }
        self.entries.get_mut(fd).ok_or(FdError::BadFd)
    }
}
