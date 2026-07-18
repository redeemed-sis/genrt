use crate::fs::fd::{FdDirEntry, FdError, FdTable};

/// Process-local descriptor and current-working-directory state.
#[derive(Copy, Clone)]
pub(super) struct ProcessFileState {
    fds: FdTable,
    cwd_dir: Option<usize>,
}

impl ProcessFileState {
    pub(super) const fn free() -> Self {
        Self {
            fds: FdTable::new(),
            cwd_dir: None,
        }
    }

    pub(super) const fn new(fds: FdTable, cwd_dir: usize) -> Self {
        Self {
            fds,
            cwd_dir: Some(cwd_dir),
        }
    }

    pub(super) fn cwd_dir(&self) -> Option<usize> {
        self.cwd_dir
    }

    pub(super) fn set_cwd(&mut self, cwd_dir: usize) {
        self.cwd_dir = Some(cwd_dir);
    }

    pub(super) fn close_all(&mut self) {
        self.fds.close_all();
        self.cwd_dir = None;
    }

    pub(super) fn snapshot(&self) -> (FdTable, Option<usize>) {
        (self.fds, self.cwd_dir)
    }

    pub(super) fn open_ram_file(&mut self, file_index: usize) -> Result<usize, FdError> {
        self.fds.open_ram_file(file_index)
    }

    pub(super) fn open_ram_dir(&mut self, dir_index: usize) -> Result<usize, FdError> {
        self.fds.open_ram_dir(dir_index)
    }

    pub(super) fn read_slice(&self, fd: usize, max_len: usize) -> Result<&'static [u8], FdError> {
        self.fds.read_slice(fd, max_len)
    }

    pub(super) fn advance_read(&mut self, fd: usize, amount: usize) -> Result<(), FdError> {
        self.fds.advance_read(fd, amount)
    }

    pub(super) fn dir_entry_at(
        &self,
        fd: usize,
        relative_offset: usize,
    ) -> Result<Option<FdDirEntry>, FdError> {
        self.fds.dir_entry_at(fd, relative_offset)
    }

    pub(super) fn advance_dir_read(&mut self, fd: usize, amount: usize) -> Result<(), FdError> {
        self.fds.advance_dir_read(fd, amount)
    }

    pub(super) fn close(&mut self, fd: usize) -> Result<(), FdError> {
        self.fds.close(fd)
    }

    pub(super) fn is_open(&self, fd: usize) -> bool {
        self.fds.is_open(fd)
    }
}
