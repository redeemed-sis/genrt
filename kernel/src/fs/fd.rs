use super::ramfs;

pub const MAX_FDS: usize = 32;
pub const STDIN: usize = 0;
pub const STDOUT: usize = 1;
pub const STDERR: usize = 2;
const FIRST_USER_FD: usize = 3;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FdError {
    BadFd,
    TooManyOpenFiles,
    Unsupported,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileHandle {
    RamFile {
        file_index: usize,
        offset: usize,
        readable: bool,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FdTable {
    entries: [Option<FileHandle>; MAX_FDS],
}

impl FdTable {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_FDS],
        }
    }

    pub fn close_all(&mut self) {
        for entry in &mut self.entries {
            *entry = None;
        }
    }

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

    pub fn close(&mut self, fd: usize) -> Result<(), FdError> {
        let entry = self.user_entry_mut(fd)?;
        if entry.is_none() {
            return Err(FdError::BadFd);
        }

        *entry = None;
        Ok(())
    }

    pub fn is_open(&self, fd: usize) -> bool {
        self.user_entry(fd).is_some_and(Option::is_some)
    }

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
            None => Err(FdError::BadFd),
        }
    }

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
