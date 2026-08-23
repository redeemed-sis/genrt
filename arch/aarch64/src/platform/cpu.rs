//! Immutable AArch64 CPU topology and PSCI startup metadata.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, Ordering},
};

use fdt_raw::Fdt;

pub(super) const MAX_BOOT_CPUS: usize = 4;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum CpuTopologyError {
    /// Immutable topology was already initialized by CPU0.
    AlreadyInitialized,
    /// DTB header or node traversal failed.
    Parse,
    /// No usable CPU node or CPU `reg` value was present.
    MissingCpu,
    /// Enabled CPU nodes exceed fixed bootstrap capacity.
    TooManyCpus,
    /// Two enabled CPU nodes normalize to the same affinity identity.
    DuplicateCpu,
    /// The executing boot CPU was absent from enabled topology.
    BootCpuMissing,
    /// PSCI metadata or CPU_ON function ID was absent.
    MissingPsci,
    /// The platform requested a conduit not supported by this QEMU target.
    UnsupportedPsciMethod,
}

#[derive(Copy, Clone)]
struct CpuTopology {
    target_ids: [u64; MAX_BOOT_CPUS],
    count: usize,
    psci_cpu_on: u64,
}

const EMPTY: u8 = 0;
const INITIALIZING: u8 = 1;
const READY: u8 = 2;

struct Publication {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<CpuTopology>>,
}

// SAFETY: CPU0 claims the sole write and publishes an immutable value with
// Release ordering. Readers dereference only after an Acquire load sees READY.
unsafe impl Sync for Publication {}

static TOPOLOGY: Publication = Publication {
    state: AtomicU8::new(EMPTY),
    value: UnsafeCell::new(MaybeUninit::uninit()),
};

/// Parse and publish immutable CPU/PSCI topology from the runtime DTB.
///
/// # Arguments
///
/// * `dtb` - Complete resident DTB byte slice.
/// * `boot_hardware_id` - Normalized executing CPU affinity key.
///
/// # Returns
///
/// Returns after one allocation-free Release publication.
///
/// # Errors
///
/// Returns [`CpuTopologyError`] for malformed, missing, duplicate, excessive,
/// or unsupported topology and for repeated initialization.
pub(crate) fn init_cpu_topology(dtb: &[u8], boot_hardware_id: u64) -> Result<(), CpuTopologyError> {
    if TOPOLOGY
        .state
        .compare_exchange(EMPTY, INITIALIZING, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return Err(CpuTopologyError::AlreadyInitialized);
    }

    let result = parse_cpu_topology(dtb, boot_hardware_id);
    match result {
        Ok(topology) => {
            // SAFETY: CPU0 exclusively owns unpublished storage after the state
            // transition above.
            unsafe { (*TOPOLOGY.value.get()).write(topology) };
            TOPOLOGY.state.store(READY, Ordering::Release);
            Ok(())
        }
        Err(err) => {
            TOPOLOGY.state.store(EMPTY, Ordering::Release);
            Err(err)
        }
    }
}

fn parse_cpu_topology(dtb: &[u8], boot_hardware_id: u64) -> Result<CpuTopology, CpuTopologyError> {
    let fdt = Fdt::from_bytes(dtb).map_err(|_| CpuTopologyError::Parse)?;
    let mut target_ids = [0u64; MAX_BOOT_CPUS];
    let mut count = 0usize;
    for node in fdt.find_children_by_path("/cpus") {
        if node.find_property_str("device_type") != Some("cpu")
            || node
                .find_property_str("status")
                .is_some_and(|status| !matches!(status, "ok" | "okay"))
        {
            continue;
        }
        let hardware = node
            .reg()
            .and_then(|mut reg| reg.next())
            .map(|entry| entry.address)
            .ok_or(CpuTopologyError::MissingCpu)?;
        if target_ids[..count]
            .iter()
            .any(|target| normalize_mpidr(*target) == normalize_mpidr(hardware))
        {
            return Err(CpuTopologyError::DuplicateCpu);
        }
        let slot = target_ids
            .get_mut(count)
            .ok_or(CpuTopologyError::TooManyCpus)?;
        *slot = hardware;
        count += 1;
    }
    if count == 0 {
        return Err(CpuTopologyError::MissingCpu);
    }

    let boot_index = target_ids[..count]
        .iter()
        .position(|hardware| normalize_mpidr(*hardware) == boot_hardware_id)
        .ok_or(CpuTopologyError::BootCpuMissing)?;
    target_ids.swap(0, boot_index);

    let psci = fdt
        .find_by_path("/psci")
        .ok_or(CpuTopologyError::MissingPsci)?;
    if psci.find_property_str("method") != Some("hvc") {
        return Err(CpuTopologyError::UnsupportedPsciMethod);
    }
    let psci_cpu_on = psci
        .find_property("cpu_on")
        .and_then(|property| property.as_u32())
        .ok_or(CpuTopologyError::MissingPsci)? as u64;

    Ok(CpuTopology {
        target_ids,
        count,
        psci_cpu_on,
    })
}

fn topology() -> &'static CpuTopology {
    if TOPOLOGY.state.load(Ordering::Acquire) != READY {
        panic!("arch: CPU topology used before publication");
    }
    // SAFETY: Acquire observed CPU0's Release publication of the immutable
    // topology.
    unsafe { (&*TOPOLOGY.value.get()).assume_init_ref() }
}

/// Return the immutable enabled CPU count.
///
/// # Returns
///
/// Returns the bounded DTB-derived count without allocation or IRQ changes.
pub(crate) fn cpu_count() -> usize {
    topology().count
}

/// Return the normalized hardware identity assigned to a logical slot.
///
/// # Arguments
///
/// * `logical_index` - CPU0-first immutable topology index.
///
/// # Returns
///
/// Returns the normalized affinity key for an in-range enabled CPU, or `None`.
pub(crate) fn expected_hardware_id(logical_index: usize) -> Option<u64> {
    let topology = topology();
    topology
        .target_ids
        .get(logical_index)
        .copied()
        .filter(|_| logical_index < topology.count)
        .map(normalize_mpidr)
}

/// Return PSCI CPU_ON metadata for one secondary logical slot.
///
/// # Arguments
///
/// * `logical_index` - CPU0-first immutable topology index.
///
/// # Returns
///
/// Returns `(function_id, raw_target_mpidr)` for an in-range CPU, or `None`.
pub(crate) fn psci_cpu_on_call(logical_index: usize) -> Option<(u64, u64)> {
    let topology = topology();
    if logical_index == 0 || logical_index >= topology.count {
        return None;
    }
    Some((
        topology.psci_cpu_on,
        *topology.target_ids.get(logical_index)?,
    ))
}

const fn normalize_mpidr(mpidr: u64) -> u64 {
    (mpidr & 0x00ff_ffff) | ((mpidr >> 8) & 0xff00_0000)
}
