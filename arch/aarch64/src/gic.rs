use core::{
    arch::asm,
    sync::atomic::{AtomicUsize, Ordering},
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
const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_IAR: usize = 0x00c;
const GICC_EOIR: usize = 0x010;
const GIC_ENABLE: u32 = 1;
const GICC_PRIORITY_ALLOW_ALL: u32 = 0xff;
const PRIVATE_IRQ_LIMIT: u32 = 32;
const CPU0_TARGET_MASK: u8 = 1;

static GICD_BASE: AtomicUsize = AtomicUsize::new(0);
static GICC_BASE: AtomicUsize = AtomicUsize::new(0);
static DISTRIBUTOR: IrqSpinLock<bool> = IrqSpinLock::new(false);

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

#[inline(always)]
unsafe fn sync_mmio() {
    // SAFETY: the barrier completes GIC MMIO programming before subsequent
    // interrupt-state publication or exception return.
    unsafe { asm!("dsb sy", "isb", options(nostack, preserves_flags)) };
}
