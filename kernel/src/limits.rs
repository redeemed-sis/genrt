/// Maximum pathname length in bytes, excluding the terminating NUL.
///
/// The limit bounds userspace pathname scans and canonical path construction.
/// It is part of the current genrt userspace ABI.
pub const GENRT_PATH_MAX: usize = 4096;
