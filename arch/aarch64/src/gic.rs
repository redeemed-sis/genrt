use core::sync::atomic::{AtomicBool, Ordering};

use crate::mmio::{mmio_read32, mmio_write8, mmio_write32};

// QEMU virt GICv2 memory map.
// Distributor (GICD) controls global interrupt configuration.
// CPU interface (GICC) is per-core path for acknowledge/EOI and priority mask.
const GICD_BASE: usize = 0x0800_0000;
const GICC_BASE: usize = 0x0801_0000;

// GICD_CTLR: distributor enable.
const GICD_CTLR: usize = GICD_BASE;
// GICD_ISENABLERn: set-enable bits for interrupt IDs (32 IDs per register).
const GICD_ISENABLER0: usize = GICD_BASE + 0x100;
// GICD_IPRIORITYRn: 8-bit priority field per interrupt ID.
const GICD_IPRIORITYR: usize = GICD_BASE + 0x400;

// GICC_CTLR: CPU interface enable.
const GICC_CTLR: usize = GICC_BASE;
// GICC_PMR: priority mask (interrupts with priority <= PMR are signaled).
const GICC_PMR: usize = GICC_BASE + 0x004;
// GICC_IAR: interrupt acknowledge register (read returns active INTID).
const GICC_IAR: usize = GICC_BASE + 0x00c;
// GICC_EOIR: end-of-interrupt register (write back IAR value).
const GICC_EOIR: usize = GICC_BASE + 0x010;

// INTID value returned when no pending interrupt is available.
const GICV2_SPURIOUS_IRQ_ID: u32 = 1023;

static GIC_INIT_DONE: AtomicBool = AtomicBool::new(false);

pub fn init_controller_minimal() {
    if GIC_INIT_DONE.load(Ordering::Relaxed) {
        return;
    }

    // SAFETY: Early boot init on boot CPU only; fixed MMIO addresses for QEMU virt GICv2.
    unsafe {
        // Enable distributor.
        mmio_write32(GICD_CTLR, 1);

        // Accept all priorities on CPU interface.
        mmio_write32(GICC_PMR, 0xff);

        // Enable CPU interface.
        mmio_write32(GICC_CTLR, 1);
    }

    GIC_INIT_DONE.store(true, Ordering::Relaxed);
}

pub fn enable_irq(irq_id: u32, priority: u8) {
    // SAFETY: MMIO writes target valid GICv2 registers on QEMU virt.
    unsafe {
        // Set interrupt priority (lower numeric value = higher priority in GIC).
        mmio_write8(GICD_IPRIORITYR + irq_id as usize, priority);

        // Enable this interrupt at distributor level.
        let bit = 1u32 << (irq_id % 32);
        mmio_write32(GICD_ISENABLER0 + ((irq_id / 32) as usize) * 4, bit);
    }
}

#[inline(always)]
pub fn acknowledge_irq() -> u32 {
    // SAFETY: Read from GICC_IAR is side-effectful by design.
    unsafe { mmio_read32(GICC_IAR) }
}

#[inline(always)]
pub fn end_irq(iar: u32) {
    // SAFETY: Write same value back to EOIR per GICv2 protocol.
    unsafe {
        mmio_write32(GICC_EOIR, iar);
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
