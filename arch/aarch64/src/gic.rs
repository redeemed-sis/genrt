use core::{
    arch::asm,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use kernel::sync::IrqSpinLock;

use crate::{
    mmio::{mmio_read32, mmio_write8, mmio_write32},
    mmu::phys_to_hva_const,
    platform::PlatformInfo,
};

// INTID value returned when no pending interrupt is available.
const GICV2_SPURIOUS_IRQ_ID: u32 = 1023;
const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER: usize = 0x100;
const GICD_IPRIORITYR: usize = 0x400;
const GICD_ITARGETSR: usize = 0x800;
const GICD_SGIR: usize = 0xf00;
const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_IAR: usize = 0x00c;
const GICC_EOIR: usize = 0x010;
const GIC_ENABLE: u32 = 1;
const GICC_PRIORITY_ALLOW_ALL: u32 = 0xff;
const PRIVATE_IRQ_LIMIT: u32 = 32;
const CPU0_TARGET_MASK: u8 = 1;

/// Private SGI reserved for scheduler remote-work notification.
pub const SCHEDULER_IPI_IRQ_ID: u32 = 1;
const SCHEDULER_IPI_PRIORITY: u8 = 0x20;
const SGIR_CPU_TARGET_LIST_SHIFT: u32 = 16;

static GICD_BASE: AtomicUsize = AtomicUsize::new(0);
static GICC_BASE: AtomicUsize = AtomicUsize::new(0);
static DISTRIBUTOR: IrqSpinLock<bool> = IrqSpinLock::new(false);
static CPU_TARGETS: [AtomicU8; crate::platform::CPU_CAPACITY] =
    [const { AtomicU8::new(0) }; crate::platform::CPU_CAPACITY];
#[cfg(feature = "qemu-test-smp-boot")]
static SCHEDULER_IPI_RECEIVED: [AtomicUsize; crate::platform::CPU_CAPACITY] =
    [const { AtomicUsize::new(0) }; crate::platform::CPU_CAPACITY];
#[cfg(feature = "qemu-test-smp-boot")]
static SCHEDULER_IPI_SENT: [AtomicUsize; crate::platform::CPU_CAPACITY] =
    [const { AtomicUsize::new(0) }; crate::platform::CPU_CAPACITY];

pub fn configure_from_platform(platform: &PlatformInfo) {
    if platform.gic_distributor.is_present() && platform.gic_cpu_interface.is_present() {
        let mut initialized = DISTRIBUTOR.lock();
        GICD_BASE.store(
            phys_to_hva_const(platform.gic_distributor.start as usize),
            Ordering::Release,
        );
        GICC_BASE.store(
            phys_to_hva_const(platform.gic_cpu_interface.start as usize),
            Ordering::Release,
        );
        *initialized = false;
    }
}

/// Initialize the shared GICv2 distributor exactly once on CPU0.
///
/// # Returns
///
/// Returns `true` when the DTB-derived distributor is enabled, or `false` when
/// GIC bases were not published. The bounded operation allocates nothing and
/// does not initialize any banked CPU-interface state.
pub fn init_global_distributor() -> bool {
    let Some((gicd, _)) = bases() else {
        return false;
    };

    let mut initialized = DISTRIBUTOR.lock();
    if !*initialized {
        // SAFETY: the distributor base came from the parsed DTB GIC `reg`
        // property. CPU0 is the sole global initializer and the lock excludes
        // any later shared SPI configuration.
        unsafe {
            mmio_write32(gicd + GICD_CTLR, GIC_ENABLE);
            sync_mmio();
        }
        *initialized = true;
    }
    unsafe { mmio_read32(gicd + GICD_CTLR) & GIC_ENABLE != 0 }
}

/// Initialize the executing CPU's banked GICv2 CPU interface.
///
/// # Returns
///
/// Returns `true` after enabling the local interface and accepting all
/// configured priorities, or `false` when GIC bases are unavailable or the
/// register readback does not match. The operation is allocation-free and
/// touches no shared distributor policy.
pub fn init_current_cpu_interface() -> bool {
    let Some((_, gicc)) = bases() else {
        return false;
    };

    // SAFETY: the CPU-interface base came from the parsed DTB GIC property.
    // GICC state is banked to the executing PE, so no shared lock is required.
    unsafe {
        mmio_write32(gicc + GICC_PMR, GICC_PRIORITY_ALLOW_ALL);
        mmio_write32(gicc + GICC_CTLR, GIC_ENABLE);
        sync_mmio();
        mmio_read32(gicc + GICC_CTLR) & GIC_ENABLE != 0
            && mmio_read32(gicc + GICC_PMR) & 0xff == GICC_PRIORITY_ALLOW_ALL
    }
}

/// Enable one banked SGI/PPI for the executing CPU.
///
/// # Arguments
///
/// * `irq_id` - Private interrupt ID in the inclusive range `0..=31`.
/// * `priority` - GIC priority byte; lower values have higher priority.
///
/// # Returns
///
/// Returns `true` when the local enable bit is observed, or `false` for an
/// invalid ID or unavailable distributor mapping. The operation is bounded,
/// allocation-free, and does not alter SPI routing.
pub fn enable_current_cpu_private_irq(irq_id: u32, priority: u8) -> bool {
    if irq_id >= PRIVATE_IRQ_LIMIT {
        return false;
    }
    let Some((gicd, _)) = bases() else {
        return false;
    };

    let bit = 1u32 << irq_id;
    // SAFETY: GICD priority and enable state for SGIs/PPIs is banked to the
    // executing CPU in GICv2. The base came from the parsed DTB.
    unsafe {
        mmio_write8(gicd + GICD_IPRIORITYR + irq_id as usize, priority);
        mmio_write32(gicd + GICD_ISENABLER, bit);
        sync_mmio();
        mmio_read32(gicd + GICD_ISENABLER) & bit != 0
    }
}

/// Initialize the executing CPU's banked scheduler SGI state.
///
/// # Returns
///
/// Returns `true` when the reserved scheduler SGI is enabled and its priority
/// is configured, or `false` when GIC distributor state is unavailable. The
/// operation is bounded, allocation-free, and touches only banked private
/// interrupt state.
pub fn init_current_cpu_scheduler_ipi() -> bool {
    enable_current_cpu_private_irq(SCHEDULER_IPI_IRQ_ID, SCHEDULER_IPI_PRIORITY)
}

/// Bind one logical CPU to the executing GICv2 CPU-interface target bit.
///
/// GICv2 target-list bits are architecture-owned interface identities, not
/// generic logical CPU indexes. A read of a banked private-interrupt target
/// field returns the one-hot target bit of the executing interface.
///
/// # Arguments
///
/// * `logical_index` - Checked logical CPU index assigned to the executing CPU.
///
/// # Returns
///
/// Returns `true` after publishing one unique one-hot target bit, or `false`
/// for an invalid index, unavailable GIC state, malformed target field, or a
/// duplicate binding. The operation is bounded and allocation-free.
pub fn bind_current_cpu_target(logical_index: usize) -> bool {
    let Some(target_slot) = CPU_TARGETS.get(logical_index) else {
        return false;
    };
    let Some((gicd, _)) = bases() else {
        return false;
    };

    // GICD_ITARGETSR0 is banked and read-only for private interrupts. In a
    // uniprocessor implementation it may be RAZ/WI, where CPU0 is necessarily
    // the only target.
    let mut target = unsafe { private_irq_target_raw(gicd, SCHEDULER_IPI_IRQ_ID) };
    if target == 0 && crate::platform::cpu_count() == 1 && logical_index == 0 {
        target = CPU0_TARGET_MASK;
    }
    if !target.is_power_of_two()
        || CPU_TARGETS
            .iter()
            .enumerate()
            .any(|(index, slot)| index != logical_index && slot.load(Ordering::Acquire) == target)
    {
        return false;
    }

    target_slot
        .compare_exchange(0, target, Ordering::Release, Ordering::Relaxed)
        .is_ok()
}

/// Send one scheduler SGI to a registered logical CPU.
///
/// # Arguments
///
/// * `logical_index` - Logical target whose architecture-owned GIC target bit
///   was published during CPU registration.
///
/// # Returns
///
/// Returns `true` after issuing the targeted GICD_SGIR write, or `false` when
/// the index is invalid, unbound, or GIC state is unavailable. The operation
/// is bounded, allocation-free, and sends to exactly one CPU interface.
pub fn send_scheduler_ipi(logical_index: usize) -> bool {
    let Some(target) = CPU_TARGETS.get(logical_index) else {
        return false;
    };
    let target = target.load(Ordering::Acquire);
    let Some((gicd, _)) = bases() else {
        return false;
    };
    if !target.is_power_of_two() {
        return false;
    }

    let value = u32::from(target) << SGIR_CPU_TARGET_LIST_SHIFT | SCHEDULER_IPI_IRQ_ID;
    // SAFETY: the distributor mapping is validated and GICD_SGIR accepts one
    // atomic targeted write. The release barrier publishes remote-ready state
    // before the target observes the SGI.
    unsafe {
        asm!("dsb ishst", options(nostack, preserves_flags));
        mmio_write32(gicd + GICD_SGIR, value);
        sync_mmio();
    }
    #[cfg(feature = "qemu-test-smp-boot")]
    SCHEDULER_IPI_SENT[logical_index].fetch_add(1, Ordering::Relaxed);
    true
}

/// Route and enable one shared peripheral interrupt on CPU0.
///
/// # Arguments
///
/// * `irq_id` - Shared interrupt ID greater than or equal to 32.
/// * `priority` - GIC priority byte; lower values have higher priority.
///
/// # Returns
///
/// Returns `true` when the SPI is enabled and has CPU0 as its only possible
/// target. SMP topologies require exact target-register readback; a one-CPU
/// topology accepts a RAZ/WI target register because CPU0 is the only possible
/// recipient. Returns `false` for an invalid ID or unavailable distributor
/// mapping. The shared mutation is serialized and allocates nothing.
pub fn enable_spi_on_cpu0(irq_id: u32, priority: u8) -> bool {
    if irq_id < PRIVATE_IRQ_LIMIT {
        return false;
    }
    let Some((gicd, _)) = bases() else {
        return false;
    };
    let _distributor = DISTRIBUTOR.lock();
    let bit = 1u32 << (irq_id % 32);
    let enable = gicd + GICD_ISENABLER + ((irq_id / 32) as usize) * 4;

    // SAFETY: shared distributor ownership is held and all addresses are
    // derived from the validated GICD mapping.
    unsafe {
        mmio_write8(gicd + GICD_ITARGETSR + irq_id as usize, CPU0_TARGET_MASK);
        mmio_write8(gicd + GICD_IPRIORITYR + irq_id as usize, priority);
        mmio_write32(enable, bit);
        sync_mmio();
        let route_ready = crate::platform::cpu_count() == 1 || spi_targets_cpu0_raw(gicd, irq_id);
        route_ready && mmio_read32(enable) & bit != 0
    }
}

#[inline(always)]
pub fn acknowledge_irq() -> u32 {
    let Some((_, gicc)) = bases() else {
        return GICV2_SPURIOUS_IRQ_ID;
    };

    // SAFETY: Read from GICC_IAR is side-effectful by design.
    unsafe { mmio_read32(gicc + GICC_IAR) }
}

#[inline(always)]
pub fn end_irq(iar: u32) {
    let Some((_, gicc)) = bases() else {
        return;
    };

    // SAFETY: Write same value back to EOIR per GICv2 protocol.
    unsafe {
        mmio_write32(gicc + GICC_EOIR, iar);
        sync_mmio();
    }
}

#[inline(always)]
pub const fn irq_id_from_iar(iar: u32) -> u32 {
    iar & 0x3ff
}

#[inline(always)]
pub const fn is_spurious(irq_id: u32) -> bool {
    irq_id == GICV2_SPURIOUS_IRQ_ID
}

#[inline(always)]
fn bases() -> Option<(usize, usize)> {
    let gicd = GICD_BASE.load(Ordering::Acquire);
    let gicc = GICC_BASE.load(Ordering::Acquire);
    (gicd != 0 && gicc != 0).then_some((gicd, gicc))
}

/// Validate that one shared interrupt remains routed exclusively to CPU0.
///
/// # Arguments
///
/// * `irq_id` - Shared interrupt ID to inspect.
///
/// # Returns
///
/// Returns `true` only when the distributor is initialized and the target byte
/// equals the CPU0 mask. This bounded diagnostic allocates nothing.
#[cfg(feature = "qemu-test-smp-boot")]
pub(crate) fn spi_targets_cpu0(irq_id: u32) -> bool {
    if irq_id < PRIVATE_IRQ_LIMIT || !*DISTRIBUTOR.lock() {
        return false;
    }
    let Some((gicd, _)) = bases() else {
        return false;
    };
    // SAFETY: the read targets the validated distributor mapping.
    unsafe { spi_targets_cpu0_raw(gicd, irq_id) }
}

unsafe fn spi_targets_cpu0_raw(gicd: usize, irq_id: u32) -> bool {
    let byte_offset = irq_id as usize;
    let word_offset = byte_offset & !3;
    let shift = (byte_offset & 3) * 8;
    // SAFETY: caller supplies the validated distributor base and an SPI ID.
    let targets = unsafe { mmio_read32(gicd + GICD_ITARGETSR + word_offset) };
    ((targets >> shift) & 0xff) as u8 == CPU0_TARGET_MASK
}

unsafe fn private_irq_target_raw(gicd: usize, irq_id: u32) -> u8 {
    let byte_offset = irq_id as usize;
    let word_offset = byte_offset & !3;
    let shift = (byte_offset & 3) * 8;
    // SAFETY: caller supplies a validated distributor base and a private IRQ.
    let targets = unsafe { mmio_read32(gicd + GICD_ITARGETSR + word_offset) };
    ((targets >> shift) & 0xff) as u8
}

/// Record one delivered scheduler SGI for the SMP QEMU contract.
///
/// # Arguments
///
/// * `logical_index` - Bound logical CPU that acknowledged the SGI.
///
/// # Returns
///
/// Returns after one bounded atomic increment. The test-only operation does
/// not allocate, block, or change interrupt-controller state.
///
/// # Panics
///
/// Panics when `logical_index` is outside fixed architecture CPU storage.
#[cfg(feature = "qemu-test-smp-boot")]
pub(crate) fn record_scheduler_ipi_for_test(logical_index: usize) {
    SCHEDULER_IPI_RECEIVED
        .get(logical_index)
        .unwrap_or_else(|| panic!("gic test: invalid scheduler IPI CPU index"))
        .fetch_add(1, Ordering::Relaxed);
}

/// Return the delivered scheduler SGI count for one logical CPU.
///
/// # Arguments
///
/// * `logical_index` - Logical CPU whose receive counter is queried.
///
/// # Returns
///
/// Returns the monotonic count, or zero for an index outside fixed storage.
/// The test-only query is bounded and allocation-free.
#[cfg(feature = "qemu-test-smp-boot")]
pub(crate) fn scheduler_ipi_received_count_for_test(logical_index: usize) -> usize {
    SCHEDULER_IPI_RECEIVED
        .get(logical_index)
        .map(|count| count.load(Ordering::Acquire))
        .unwrap_or(0)
}

/// Return the scheduler SGI send count for one logical destination.
///
/// # Arguments
///
/// * `logical_index` - Logical CPU whose targeted-send counter is queried.
///
/// # Returns
///
/// Returns the monotonic count of successful SGIR writes targeting that CPU,
/// or zero for an index outside fixed storage. The test-only query is bounded
/// and allocation-free.
#[cfg(feature = "qemu-test-smp-boot")]
pub(crate) fn scheduler_ipi_sent_count_for_test(logical_index: usize) -> usize {
    SCHEDULER_IPI_SENT
        .get(logical_index)
        .map(|count| count.load(Ordering::Acquire))
        .unwrap_or(0)
}

#[inline(always)]
unsafe fn sync_mmio() {
    // SAFETY: the barrier completes GIC MMIO programming before subsequent
    // interrupt-state publication or exception return.
    unsafe { asm!("dsb sy", "isb", options(nostack, preserves_flags)) };
}
