use crate::{cpu::CpuId, errno, sync::LocalIrqGuard};

use super::{
    id::ProcessId,
    table::{current_process_id, table_mut},
};

const USER_PID_MAX: isize = i32::MAX as isize;

/// Return the immutable CPU owner of one live process.
///
/// The lookup uses the generation-bearing process ID and holds process-table
/// ownership only long enough to copy the owner. It does not allocate or block.
///
/// # Arguments
///
/// * `raw_pid` - `0` for the current process, or a positive PID returned by
///   `fork`.
///
/// # Returns
///
/// Returns the logical CPU that owns the process, its main thread, and its
/// address space.
///
/// # Errors
///
/// Returns [`errno::ESRCH`] when the PID is negative, malformed, stale, absent,
/// not yet published, or when the caller requests its current process outside
/// userspace.
///
/// # Panics
///
/// Panics if a published process lacks an address space or its process and
/// address-space owners differ. Either condition violates immutable ownership.
pub(crate) fn process_affinity(raw_pid: isize) -> Result<CpuId, errno::Errno> {
    let pid = match raw_pid {
        0 => current_process_id().ok_or(errno::ESRCH)?,
        value if value < 0 || value > USER_PID_MAX => return Err(errno::ESRCH),
        value => ProcessId::from_raw(value as usize).ok_or(errno::ESRCH)?,
    };

    let _irq_guard = LocalIrqGuard::save_and_disable();
    let table = table_mut();
    let slot = table.slot(pid).ok_or(errno::ESRCH)?;
    if slot.process.main_thread.is_none() {
        return Err(errno::ESRCH);
    }
    let owner = slot.process.owner_cpu();
    let address_owner = slot
        .process
        .resources
        .image
        .address_space
        .as_ref()
        .unwrap_or_else(|| panic!("process: published process lacks an address space"))
        .id()
        .owner_cpu();
    if owner != address_owner {
        panic!("process: published process has inconsistent CPU ownership");
    }
    Ok(owner)
}
