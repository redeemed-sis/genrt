//! Logical CPU identity, boot-time hardware registration, and runtime binding.
//!
//! The active kernel executes only on the boot CPU.  This module still makes
//! that CPU's logical identity explicit so scheduler execution-local state can
//! be partitioned before secondary CPUs are brought up.

use core::cell::UnsafeCell;

use crate::config::KERNEL_CPU_CAPACITY;

unsafe extern "C" {
    fn arch_current_cpu_hardware_id() -> u64;
    fn arch_bind_current_cpu_logical_id(logical_index: usize);
    fn arch_current_cpu_logical_id() -> usize;
}

/// Bounded logical CPU identity used by generic kernel code.
///
/// Logical IDs index fixed-capacity kernel storage. Generic code can enumerate
/// checked reserved slots, but only the boot registry makes an identity
/// registered and therefore eligible for execution.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CpuId(usize);

impl CpuId {
    /// Construct a logical ID for one bounded per-CPU storage slot.
    ///
    /// # Arguments
    ///
    /// * `index` - Candidate index in fixed logical-CPU storage.
    ///
    /// # Returns
    ///
    /// Returns `Some(CpuId)` when `index` is below
    /// [`KERNEL_CPU_CAPACITY`], or `None` otherwise. This checked constructor
    /// does not register a CPU and must not be used to reinterpret a hardware
    /// identity.
    pub(crate) const fn from_index(index: usize) -> Option<Self> {
        if index < KERNEL_CPU_CAPACITY {
            Some(Self(index))
        } else {
            None
        }
    }

    /// Return this CPU's checked fixed-storage index.
    ///
    /// # Returns
    ///
    /// Returns an index strictly smaller than `KERNEL_CPU_CAPACITY`. This is
    /// a copy-only operation that does not allocate, block, or alter IRQ state.
    pub const fn index(self) -> usize {
        self.0
    }
}

/// Opaque normalized hardware CPU identity supplied by the architecture layer.
///
/// Generic kernel code compares this value only during boot registration. It
/// never interprets architecture register fields or treats the key as a
/// logical index.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct HardwareCpuId(u64);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
/// Errors from bounded logical CPU registration and current-CPU resolution.
///
/// These copy-only diagnostics allocate nothing and carry no architecture
/// register encoding.
pub(crate) enum CpuRegistrationError {
    /// The hardware identity is already present in the registry.
    AlreadyRegistered,
    /// Fixed logical CPU capacity has been exhausted.
    CapacityExceeded,
    /// The executing hardware CPU has no logical registration.
    UnknownCurrentCpu,
}

#[derive(Copy, Clone)]
struct CpuRegistry {
    hardware: [Option<HardwareCpuId>; KERNEL_CPU_CAPACITY],
    count: usize,
}

impl CpuRegistry {
    const fn new() -> Self {
        Self {
            hardware: [None; KERNEL_CPU_CAPACITY],
            count: 0,
        }
    }

    fn register(&mut self, hardware: HardwareCpuId) -> Result<CpuId, CpuRegistrationError> {
        if self.hardware[..self.count].contains(&Some(hardware)) {
            return Err(CpuRegistrationError::AlreadyRegistered);
        }
        if self.count == KERNEL_CPU_CAPACITY {
            return Err(CpuRegistrationError::CapacityExceeded);
        }
        let id = CpuId(self.count);
        self.hardware[self.count] = Some(hardware);
        self.count += 1;
        Ok(id)
    }
}

struct CpuRegistryCell(UnsafeCell<CpuRegistry>);

// SAFETY: registration happens before the first scheduler entry.  Afterwards
// the active system executes kernel code only on CPU0; any SMP enablement must
// replace this with synchronization before a second CPU may access the table.
unsafe impl Sync for CpuRegistryCell {}

static REGISTRY: CpuRegistryCell = CpuRegistryCell(UnsafeCell::new(CpuRegistry::new()));

#[inline(always)]
fn registry_mut() -> &'static mut CpuRegistry {
    // SAFETY: boot registration is the only mutable access and completes
    // before runtime or interrupt-side registry readers exist.
    unsafe { &mut *REGISTRY.0.get() }
}

#[inline(always)]
fn registry() -> &'static CpuRegistry {
    // SAFETY: the registry is immutable after boot registration. Shared
    // runtime reads may therefore overlap across thread and IRQ context.
    unsafe { &*REGISTRY.0.get() }
}

#[inline(always)]
fn current_hardware_id() -> HardwareCpuId {
    // SAFETY: the architecture hook is a register-only normalized MPIDR read.
    HardwareCpuId(unsafe { arch_current_cpu_hardware_id() })
}

/// Register the executing boot CPU as logical CPU0.
///
/// This must run once before scheduler bootstrap and before any code relies on
/// CPU-local scheduler/preemption state.  It performs a bounded register read
/// and fixed-array update; it neither allocates nor changes IRQ state.
///
/// # Returns
///
/// Returns logical CPU0 after the first successful registration.
///
/// # Errors
///
/// Returns [`CpuRegistrationError::AlreadyRegistered`] when invoked twice, or
/// [`CpuRegistrationError::CapacityExceeded`] if the fixed registry is full.
pub(crate) fn register_boot_cpu() -> Result<CpuId, CpuRegistrationError> {
    let registry = registry_mut();
    if registry.count != 0 {
        return Err(CpuRegistrationError::AlreadyRegistered);
    }
    let id = registry.register(current_hardware_id())?;
    // SAFETY: `register` returned a checked logical ID owned by the executing
    // hardware CPU, and boot registration runs before runtime readers exist.
    unsafe { arch_bind_current_cpu_logical_id(id.index()) };
    Ok(id)
}

/// Resolve the executing CPU's registered logical identity.
///
/// The architecture layer reads the logical binding installed at registration,
/// so resolution is O(1), allocation-free, and does not alter IRQ state.
/// Unbound, out-of-range, and unpublished identities are explicit errors and
/// are never silently treated as CPU0.
///
/// # Returns
///
/// Returns the registered logical identity of the executing CPU.
///
/// # Errors
///
/// Returns [`CpuRegistrationError::UnknownCurrentCpu`] if the architecture
/// hook reports no binding or a logical identity that boot registration did
/// not publish.
pub(crate) fn current_id() -> Result<CpuId, CpuRegistrationError> {
    // SAFETY: the architecture hook returns only the software-owned encoded
    // logical binding and performs no memory access through caller pointers.
    let encoded = unsafe { arch_current_cpu_logical_id() };
    let index = encoded
        .checked_sub(1)
        .ok_or(CpuRegistrationError::UnknownCurrentCpu)?;
    let id = CpuId::from_index(index).ok_or(CpuRegistrationError::UnknownCurrentCpu)?;
    is_registered(id)
        .then_some(id)
        .ok_or(CpuRegistrationError::UnknownCurrentCpu)
}

/// Return whether `cpu` is currently registered in the bounded hardware map.
///
/// # Arguments
///
/// * `cpu` - Checked logical CPU identity to test.
///
/// # Returns
///
/// Returns `true` only when boot registration published this logical CPU. The
/// direct count check allocates nothing and does not alter IRQ state.
pub(crate) fn is_registered(cpu: CpuId) -> bool {
    cpu.index() < registry().count
}

/// Return the number of registered logical CPUs.
///
/// # Returns
///
/// Returns the bounded registry count without allocation, blocking, or IRQ
/// state changes.
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) fn registered_count() -> usize {
    registry().count
}

/// Validate the boot CPU registry contract for the QEMU kernel coordinator.
///
/// The check performs constant-time architecture binding reads only. It
/// allocates nothing, does not block, and leaves IRQ state unchanged.
///
/// # Returns
///
/// Returns after confirming that exactly one CPU is registered and repeated
/// current-CPU resolution yields logical CPU0.
///
/// # Panics
///
/// Panics when the registry count or current logical identity violates the
/// boot-CPU contract.
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) fn validate_boot_cpu_for_test() {
    let first = current_id().unwrap_or_else(|err| panic!("cpu test: lookup failed: {err:?}"));
    let second = current_id().unwrap_or_else(|err| panic!("cpu test: lookup failed: {err:?}"));
    if first != CpuId(0) || second != first || registered_count() != 1 {
        panic!("cpu test: unstable logical boot CPU identity");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_assigns_cpu_zero_and_rejects_duplicates_and_overflow() {
        let mut registry = CpuRegistry::new();
        assert_eq!(registry.register(HardwareCpuId(11)), Ok(CpuId(0)));
        assert_eq!(
            registry.register(HardwareCpuId(11)),
            Err(CpuRegistrationError::AlreadyRegistered)
        );
        for raw in 12..(12 + KERNEL_CPU_CAPACITY as u64 - 1) {
            assert!(registry.register(HardwareCpuId(raw)).is_ok());
        }
        assert_eq!(
            registry.register(HardwareCpuId(99)),
            Err(CpuRegistrationError::CapacityExceeded)
        );
    }
}
