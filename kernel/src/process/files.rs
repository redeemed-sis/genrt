use crate::fs::{call::PendingFileCall, fd::FdTable, vfs::Location};

/// Process ownership; only clone/take/move these fields under the table lock.
pub(super) struct ProcessFileState {
    pub(super) fds: FdTable,
    pub(super) cwd: Option<Location>,
    pub(super) pending: Option<PendingFileCall>,
}

// Keep process-table moves and lifecycle stack usage bounded. I/O bytes live
// only in the heap-preallocated OFD, never in this inline continuation.
const _: () = assert!(core::mem::size_of::<PendingFileCall>() <= 256);
const _: () = assert!(core::mem::size_of::<ProcessFileState>() <= 640);
const _: () = assert!(core::mem::size_of::<super::table::ProcessSlot>() <= 1024);
const _: () = assert!(core::mem::size_of::<super::table::ProcessTable>() <= 4096);

impl ProcessFileState {
    /// Test whether final owning resources have already been extracted.
    /// # Returns
    /// True if descriptors, cwd and continuation are empty; no destruction.
    pub(super) fn is_empty(&self) -> bool {
        self.fds.is_empty() && self.cwd.is_none() && self.pending.is_none()
    }

    /// Construct an empty ownership bundle without allocation or destruction.
    /// # Returns
    /// File state suitable for a free or terminal slot.
    pub(super) const fn free() -> Self {
        Self {
            fds: FdTable::new(),
            cwd: None,
            pending: None,
        }
    }

    /// Move staged file ownership into a process without backend work.
    /// # Arguments
    /// * `fds` - Descriptor references inherited or staged outside table locks.
    /// * `cwd` - Pinned directory, or none for synthetic host metadata tests.
    /// # Returns
    /// File state without a pending syscall; no allocation or destruction.
    pub(super) const fn new(fds: FdTable, cwd: Option<Location>) -> Self {
        Self {
            fds,
            cwd,
            pending: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_file_metadata_stays_small() {
        let sizes = (
            core::mem::size_of::<crate::fs::fd::Transaction>(),
            core::mem::size_of::<PendingFileCall>(),
            core::mem::size_of::<ProcessFileState>(),
            core::mem::size_of::<super::super::table::ProcessSlot>(),
            core::mem::size_of::<super::super::table::ProcessTable>(),
        );
        std::println!(
            "Transaction/PendingFileCall/ProcessFileState/ProcessSlot/ProcessTable bytes: {sizes:?}"
        );
        assert!(sizes.0 <= 128);
        assert!(sizes.1 <= 256);
        assert!(sizes.2 <= 640);
        assert!(sizes.3 <= 1024);
        assert!(sizes.4 <= 4096);
    }
}
