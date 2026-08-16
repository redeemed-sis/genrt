//! Logical CPU identity, boot-time hardware registration, and parked-secondary
//! readiness.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{config::KERNEL_CPU_CAPACITY, sync::IrqSpinLock};

unsafe extern "C" {
    fn arch_current_cpu_hardware_id() -> u64;
    fn arch_bind_current_cpu_logical_id(logical_index: usize);
    fn arch_current_cpu_logical_id() -> usize;
    fn arch_expected_cpu_count() -> usize;
    fn arch_bootstrap_stack_capacity() -> usize;
    fn arch_secondary_cpu_identity_matches(logical_index: usize) -> bool;
    fn arch_start_secondary_cpu(logical_index: usize) -> i64;
}

const SECONDARY_START_TIMEOUT_MS: u64 = 5_000;

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
    /// The platform described no CPUs or more CPUs than fixed kernel storage.
    InvalidExpectedCount,
    /// A secondary CPU arrived before CPU0 published platform topology.
    ExpectedCountNotConfigured,
    /// More hardware CPUs arrived than the platform topology declared.
    UnexpectedCpu,
    /// The architecture supplied a bootstrap stack slot outside fixed storage.
    InvalidBootstrapSlot,
    /// Two hardware CPUs reported ownership of the same bootstrap stack slot.
    BootstrapSlotInUse,
    /// Architecture topology does not match generic boot information.
    TopologyMismatch,
    /// PSCI or the active architecture rejected a CPU startup request.
    ArchitectureStartFailed,
    /// A started CPU reported a registration or identity failure.
    SecondaryStartupFailed,
    /// A started CPU did not publish parked readiness before the boot deadline.
    SecondaryStartupTimeout,
    /// The executing hardware CPU has no logical registration.
    UnknownCurrentCpu,
}

#[derive(Copy, Clone)]
struct CpuRecord {
    hardware: HardwareCpuId,
    bootstrap_slot: usize,
    current_id_verified: bool,
    parked: bool,
}

#[derive(Copy, Clone)]
struct CpuRegistry {
    records: [Option<CpuRecord>; KERNEL_CPU_CAPACITY],
    count: usize,
    expected: usize,
}

impl CpuRegistry {
    const fn new() -> Self {
        Self {
            records: [None; KERNEL_CPU_CAPACITY],
            count: 0,
            expected: 0,
        }
    }

    fn register(
        &mut self,
        hardware: HardwareCpuId,
        bootstrap_slot: usize,
    ) -> Result<CpuId, CpuRegistrationError> {
        if self.count == KERNEL_CPU_CAPACITY {
            return Err(CpuRegistrationError::CapacityExceeded);
        }
        if bootstrap_slot >= KERNEL_CPU_CAPACITY {
            return Err(CpuRegistrationError::InvalidBootstrapSlot);
        }
        if self.records[..self.count]
            .iter()
            .flatten()
            .any(|record| record.hardware == hardware)
        {
            return Err(CpuRegistrationError::AlreadyRegistered);
        }
        if self.records[..self.count]
            .iter()
            .flatten()
            .any(|record| record.bootstrap_slot == bootstrap_slot)
        {
            return Err(CpuRegistrationError::BootstrapSlotInUse);
        }
        let id = CpuId(self.count);
        self.records[self.count] = Some(CpuRecord {
            hardware,
            bootstrap_slot,
            current_id_verified: false,
            parked: false,
        });
        self.count += 1;
        Ok(id)
    }

    fn record_mut(&mut self, id: CpuId) -> Option<&mut CpuRecord> {
        self.records.get_mut(id.index())?.as_mut()
    }
}

static REGISTRY: IrqSpinLock<CpuRegistry> = IrqSpinLock::new(CpuRegistry::new());
static REGISTERED_COUNT: AtomicUsize = AtomicUsize::new(0);
static EXPECTED_COUNT: AtomicUsize = AtomicUsize::new(0);
static STARTUP_FAILED: AtomicUsize = AtomicUsize::new(0);

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
    let mut registry = REGISTRY.lock();
    if registry.count != 0 {
        return Err(CpuRegistrationError::AlreadyRegistered);
    }
    let id = registry.register(current_hardware_id(), 0)?;
    // SAFETY: `register` returned a checked logical ID owned by the executing
    // hardware CPU, and boot registration runs before runtime readers exist.
    unsafe { arch_bind_current_cpu_logical_id(id.index()) };
    REGISTERED_COUNT.store(registry.count, Ordering::Release);
    registry
        .record_mut(id)
        .expect("cpu: boot CPU record disappeared")
        .current_id_verified = true;
    Ok(id)
}

/// Publish the number of CPUs described by immutable boot platform data.
///
/// CPU0 calls this after its own registration and before secondary CPUs may
/// register. Publication is allocation-free and uses a short IRQ-safe spinlock
/// section followed by a Release store.
///
/// # Arguments
///
/// * `expected` - Number of enabled CPUs described by the boot platform,
///   including CPU0.
///
/// # Returns
///
/// Returns after publishing a valid count exactly once.
///
/// # Errors
///
/// Returns [`CpuRegistrationError::InvalidExpectedCount`] for zero or a value
/// above fixed kernel capacity, or [`CpuRegistrationError::AlreadyRegistered`]
/// when topology was already published.
pub(crate) fn configure_expected_count(expected: usize) -> Result<(), CpuRegistrationError> {
    // SAFETY: this architecture hook returns a linker-derived integer and has
    // no side effects or pointer arguments.
    let stack_capacity = unsafe { arch_bootstrap_stack_capacity() };
    if expected == 0 || expected > KERNEL_CPU_CAPACITY || stack_capacity != KERNEL_CPU_CAPACITY {
        return Err(CpuRegistrationError::InvalidExpectedCount);
    }
    // SAFETY: the architecture hook reads immutable topology published before
    // generic kernel entry and has no caller-owned pointer arguments.
    if unsafe { arch_expected_cpu_count() } != expected {
        return Err(CpuRegistrationError::TopologyMismatch);
    }
    let mut registry = REGISTRY.lock();
    if registry.expected != 0 {
        return Err(CpuRegistrationError::AlreadyRegistered);
    }
    if registry.count != 1 || expected < registry.count {
        return Err(CpuRegistrationError::InvalidExpectedCount);
    }
    registry.expected = expected;
    EXPECTED_COUNT.store(expected, Ordering::Release);
    Ok(())
}

/// Register the executing secondary CPU and install its runtime logical binding.
///
/// # Arguments
///
/// * `logical_index` - CPU0-selected logical slot passed through PSCI and also
///   used as the private bootstrap-stack slot.
///
/// # Returns
///
/// Returns the next registry-assigned logical ID after binding it to the
/// executing CPU and verifying [`current_id`]. The operation is bounded,
/// allocation-free, and scheduler-independent.
///
/// # Errors
///
/// Returns a [`CpuRegistrationError`] for duplicate hardware/stack identity,
/// invalid or exhausted capacity, an unexpected extra CPU, or failed runtime
/// binding validation.
pub(crate) fn register_secondary_cpu(logical_index: usize) -> Result<CpuId, CpuRegistrationError> {
    let expected = EXPECTED_COUNT.load(Ordering::Acquire);
    if expected == 0 {
        return Err(CpuRegistrationError::ExpectedCountNotConfigured);
    }
    // SAFETY: the architecture compares the executing hardware identity with
    // immutable topology for the CPU0-selected logical slot.
    if !unsafe { arch_secondary_cpu_identity_matches(logical_index) } {
        return Err(CpuRegistrationError::UnknownCurrentCpu);
    }
    let id = {
        let mut registry = REGISTRY.lock();
        if registry.expected == 0 {
            return Err(CpuRegistrationError::ExpectedCountNotConfigured);
        }
        if registry.count >= expected || registry.count != logical_index {
            return Err(CpuRegistrationError::UnexpectedCpu);
        }
        let id = registry.register(current_hardware_id(), logical_index)?;
        // SAFETY: the registry exclusively assigned this checked logical ID to
        // the executing hardware CPU.
        unsafe { arch_bind_current_cpu_logical_id(id.index()) };
        REGISTERED_COUNT.store(registry.count, Ordering::Release);
        id
    };

    if current_id()? != id {
        return Err(CpuRegistrationError::UnknownCurrentCpu);
    }
    let mut registry = REGISTRY.lock();
    registry
        .record_mut(id)
        .ok_or(CpuRegistrationError::UnknownCurrentCpu)?
        .current_id_verified = true;
    Ok(id)
}

/// Start every platform-described secondary CPU and wait for parked readiness.
///
/// CPU0 invokes PSCI sequentially so logical registration order is stable and
/// each started CPU either reaches its exact parked state or terminates global
/// boot with a controlled error. This path allocates nothing and does not use
/// the scheduler.
///
/// # Returns
///
/// Returns after every expected secondary CPU has registered, verified its
/// runtime identity, and published parked state. A single-core topology returns
/// immediately.
///
/// # Errors
///
/// Returns [`CpuRegistrationError::ArchitectureStartFailed`] when PSCI rejects
/// CPU_ON, [`CpuRegistrationError::SecondaryStartupFailed`] after a secondary
/// reports failure, or [`CpuRegistrationError::SecondaryStartupTimeout`] when
/// readiness misses the bounded deadline.
pub(crate) fn start_and_park_secondaries() -> Result<(), CpuRegistrationError> {
    let expected = EXPECTED_COUNT.load(Ordering::Acquire);
    if expected == 0 {
        return Err(CpuRegistrationError::ExpectedCountNotConfigured);
    }
    for logical_index in 1..expected {
        // SAFETY: CPU0 supplies a checked slot from immutable published
        // topology; the architecture owns PSCI and entry-address details.
        if unsafe { arch_start_secondary_cpu(logical_index) } != 0 {
            return Err(CpuRegistrationError::ArchitectureStartFailed);
        }
        let deadline = crate::time::uptime_ms().saturating_add(SECONDARY_START_TIMEOUT_MS);
        loop {
            if STARTUP_FAILED.load(Ordering::Acquire) != 0 {
                return Err(CpuRegistrationError::SecondaryStartupFailed);
            }
            let parked = {
                let registry = REGISTRY.lock();
                registry
                    .records
                    .get(logical_index)
                    .and_then(Option::as_ref)
                    .is_some_and(|record| record.parked)
            };
            if parked {
                break;
            }
            if crate::time::uptime_ms() >= deadline {
                return Err(CpuRegistrationError::SecondaryStartupTimeout);
            }
            core::hint::spin_loop();
        }
    }
    Ok(())
}

/// Publish that the executing secondary CPU reached its terminal parked boundary.
///
/// # Arguments
///
/// * `id` - Logical ID returned by [`register_secondary_cpu`].
///
/// # Returns
///
/// Returns after a bounded Release-visible registry update.
///
/// # Errors
///
/// Returns [`CpuRegistrationError::UnknownCurrentCpu`] if `id` is not the
/// executing registered CPU or its record is unavailable.
pub(crate) fn mark_secondary_parked(id: CpuId) -> Result<(), CpuRegistrationError> {
    if current_id()? != id || id.index() == 0 {
        return Err(CpuRegistrationError::UnknownCurrentCpu);
    }
    let mut registry = REGISTRY.lock();
    let record = registry
        .record_mut(id)
        .ok_or(CpuRegistrationError::UnknownCurrentCpu)?;
    if !record.current_id_verified {
        return Err(CpuRegistrationError::UnknownCurrentCpu);
    }
    record.parked = true;
    Ok(())
}

/// Record a secondary startup failure for CPU0 diagnostics and test gating.
///
/// # Returns
///
/// Returns after a Release store. The operation is allocation-free,
/// scheduler-independent, and safe before logical CPU registration.
pub(crate) fn record_secondary_startup_failure() {
    STARTUP_FAILED.store(1, Ordering::Release);
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
    cpu.index() < REGISTERED_COUNT.load(Ordering::Acquire)
}

/// Return the number of registered logical CPUs.
///
/// # Returns
///
/// Returns the bounded registry count without allocation, blocking, or IRQ
/// state changes.
#[cfg(any(feature = "qemu-test-kernel-runtime", feature = "qemu-test-smp-boot"))]
pub(crate) fn registered_count() -> usize {
    REGISTERED_COUNT.load(Ordering::Acquire)
}

/// Return whether every platform-described secondary CPU is registered and parked.
///
/// # Returns
///
/// Returns `true` only when startup has not failed, the registry count matches
/// the expected topology, and every non-boot record has published parked state.
/// The bounded query allocates nothing and does not block the scheduler.
#[cfg(feature = "qemu-test-smp-boot")]
pub(crate) fn secondary_boot_complete() -> bool {
    if STARTUP_FAILED.load(Ordering::Acquire) != 0 {
        return false;
    }
    let registry = REGISTRY.lock();
    registry.expected > 1
        && registry.count == registry.expected
        && registry.records[1..registry.count]
            .iter()
            .flatten()
            .all(|record| record.parked)
}

/// Validate the complete parked-secondary boot contract for QEMU.
///
/// # Returns
///
/// Returns after checking expected/registered counts, unique registry-owned
/// hardware and bootstrap-stack identities, per-CPU current-ID validation, and
/// parked state. The bounded check allocates nothing.
///
/// # Panics
///
/// Panics when any SMP boot invariant is violated.
#[cfg(feature = "qemu-test-smp-boot")]
pub(crate) fn validate_smp_boot_for_test() {
    if STARTUP_FAILED.load(Ordering::Acquire) != 0 {
        panic!("cpu test: secondary startup failure was recorded");
    }
    if current_id() != Ok(CpuId(0)) {
        panic!("cpu test: boot CPU lost logical CPU0 identity");
    }
    let registry = REGISTRY.lock();
    if registry.expected <= 1 || registry.count != registry.expected {
        panic!("cpu test: expected CPU topology was not fully registered");
    }
    for index in 0..registry.count {
        let record = registry.records[index]
            .unwrap_or_else(|| panic!("cpu test: missing registered CPU record"));
        if !record.current_id_verified || (index != 0 && !record.parked) {
            panic!("cpu test: CPU did not verify identity and reach its target state");
        }
        for prior in registry.records[..index].iter().flatten() {
            if prior.hardware == record.hardware || prior.bootstrap_slot == record.bootstrap_slot {
                panic!("cpu test: duplicate hardware identity or bootstrap stack");
            }
        }
    }
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
        assert_eq!(registry.register(HardwareCpuId(11), 0), Ok(CpuId(0)));
        assert_eq!(
            registry.register(HardwareCpuId(11), 1),
            Err(CpuRegistrationError::AlreadyRegistered)
        );
        for (stack, raw) in (12..(12 + KERNEL_CPU_CAPACITY as u64 - 1)).enumerate() {
            assert!(registry.register(HardwareCpuId(raw), stack + 1).is_ok());
        }
        assert_eq!(
            registry.register(HardwareCpuId(99), KERNEL_CPU_CAPACITY),
            Err(CpuRegistrationError::CapacityExceeded)
        );
    }

    #[test]
    fn registry_rejects_duplicate_bootstrap_slot() {
        let mut registry = CpuRegistry::new();
        assert_eq!(registry.register(HardwareCpuId(1), 0), Ok(CpuId(0)));
        assert_eq!(
            registry.register(HardwareCpuId(2), 0),
            Err(CpuRegistrationError::BootstrapSlotInUse)
        );
        assert_eq!(
            registry.register(HardwareCpuId(3), KERNEL_CPU_CAPACITY),
            Err(CpuRegistrationError::InvalidBootstrapSlot)
        );
    }
}
