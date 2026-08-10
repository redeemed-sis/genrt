use core::sync::atomic::{AtomicUsize, Ordering};

use kernel::sync::IrqSpinLock;

use crate::{
    mmio::{mmio_read32, mmio_write8, mmio_write32},
    mmu::phys_to_hva_const,
    platform::PlatformInfo,
};

// INTID value returned when no pending interrupt is available.
const GICV2_SPURIOUS_IRQ_ID: u32 = 1023;

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

pub fn init_controller_minimal() {
    let Some((gicd, gicc)) = bases() else {
        return;
    };

    {
        let mut initialized = DISTRIBUTOR.lock();
        if !*initialized {
            // SAFETY: the distributor base came from the parsed DTB GIC
            // `reg` property. The global lock serializes shared distributor
            // initialization across CPUs.
            unsafe { mmio_write32(gicd, 1) };
            *initialized = true;
        }
    }

    // GICC is banked CPU-interface state. Each CPU initializes its own view;
    // no global distributor lock is needed around these local MMIO writes.
    // SAFETY: the CPU-interface base came from the parsed DTB GIC property.
    unsafe {
        // Accept all priorities on CPU interface.
        mmio_write32(gicc + 0x004, 0xff);

        // Enable CPU interface.
        mmio_write32(gicc, 1);
    }
}

pub fn enable_irq(irq_id: u32, priority: u8) {
    let Some((gicd, _)) = bases() else {
        return;
    };
    let _distributor = DISTRIBUTOR.lock();

    // SAFETY: GIC base addresses came from the parsed DTB GIC `reg` property.
    unsafe {
        // Route SPIs to CPU0 in the current single-core QEMU virt milestone.
        // SGIs/PPIs use banked targeting and do not have writable ITARGETSR
        // entries in the same way.
        if irq_id >= 32 {
            mmio_write8(gicd + 0x800 + irq_id as usize, 0x01);
        }

        // Set interrupt priority (lower numeric value = higher priority in GIC).
        mmio_write8(gicd + 0x400 + irq_id as usize, priority);

        // Enable this interrupt at distributor level.
        let bit = 1u32 << (irq_id % 32);
        mmio_write32(gicd + 0x100 + ((irq_id / 32) as usize) * 4, bit);
    }
}

#[inline(always)]
pub fn acknowledge_irq() -> u32 {
    let Some((_, gicc)) = bases() else {
        return GICV2_SPURIOUS_IRQ_ID;
    };

    // SAFETY: Read from GICC_IAR is side-effectful by design.
    unsafe { mmio_read32(gicc + 0x00c) }
}

#[inline(always)]
pub fn end_irq(iar: u32) {
    let Some((_, gicc)) = bases() else {
        return;
    };

    // SAFETY: Write same value back to EOIR per GICv2 protocol.
    unsafe {
        mmio_write32(gicc + 0x010, iar);
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
