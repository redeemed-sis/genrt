//! BCM2837 runtime: CPU0 GPU IRQ cascade and per-CPU timer/mailbox.
//! Register layout: BCM2835 ARM Peripherals and BCM2836 ARM-local peripherals.
//! No GIC or PSCI is assumed. Firmware spin-table and watchdog live separately.

use super::{BootPlatformInfo, PlatformInfo};
use crate::{
    console,
    interrupts::{Checkpoint, InterruptController, IrqEvents},
    mmio, mmu,
};

const LOCAL: usize = 0x4000_0000;
const GPU: usize = 0x3f00_b200;
const TIMER_PENDING: u32 = 1 << 1; // Non-secure physical timer.
const MAILBOX_PENDING: u32 = 1 << 4; // Current CPU mailbox 0.
const GPU_PENDING: u32 = 1 << 8;

/// BCM2837 local and GPU interrupt controller selected for Pi boot.
pub(crate) struct Controller;

/// Add the ARM-local interrupt window after the DTB supplies the SoC MMIO range.
/// Returns nothing; pre-MMU and allocation-free. Only the primary interrupt
/// slot is used; the DTB-mapped SoC range covers UART and GPU IRQ.

#[unsafe(link_section = ".boot.text")]
pub(crate) fn boot_ranges(info: &mut BootPlatformInfo) {
    info.irq_primary.start = LOCAL;
    info.irq_primary.size = 0x1000;
    info.irq_secondary.start = 0;
    info.irq_secondary.size = 0;
}

fn read(pa: usize) -> u32 {
    // SAFETY: these fixed BCM2837 registers have boot and runtime Device aliases.
    unsafe { mmio::mmio_read32(mmu::phys_to_hva(pa)) }
}
fn write(pa: usize, value: u32) {
    // SAFETY: CPU0 owns these controller registers with local IRQ masked.
    unsafe { mmio::mmio_write32(mmu::phys_to_hva(pa), value) }
}
impl InterruptController for Controller {
    /// Initialize CPU0 interrupt routing; `_platform` has already mapped MMIO.
    /// Returns nothing. CPU0-only, IRQ-masked and allocation-free before CPU release.
    fn init_global(_platform: &PlatformInfo) {
        write(GPU + 0x1c, u32::MAX);
        write(GPU + 0x20, u32::MAX);
        write(GPU + 0x24, u32::MAX);
        write(GPU + 0x0c, 0); // Disable GPU FIQ selection.
        write(LOCAL + 0x0c, 0); // Route GPU IRQ to CPU0.
        write(LOCAL, 0); // Crystal source, increment by one.
        write(LOCAL + 8, 0x8000_0000); // 1:1 architected-counter prescaler.
        console::enable_rx_interrupts();
        let irq = super::uart::bcm2837::runtime::gpu_irq();
        write(GPU + 0x10 + (irq as usize / 32) * 4, 1 << (irq % 32));
    }

    /// Enable the executing CPU's CNTPNSIRQ and mailbox0; return true on success.
    /// No arguments. Boot-only with IRQ masked, allocation-free; rejects CPUs >= 4.
    fn init_local() -> bool {
        let cpu = crate::arch_current_cpu_hardware_id() as usize;
        if cpu >= 4 {
            return false;
        }
        write(LOCAL + 0x40 + cpu * 4, TIMER_PENDING);
        write(LOCAL + 0xc0 + cpu * 16, u32::MAX);
        write(LOCAL + 0x50 + cpu * 4, 1);
        true
    }

    /// Accept logical CPU `index` matching its validated hardware identity, below 4.
    /// Returns whether the binding is valid.
    /// This bounded check does not allocate or alter IRQ state.
    fn bind_cpu(index: usize) -> bool {
        index < 4 && crate::arch_current_cpu_hardware_id() as usize == index
    }

    /// Send a notification to logical CPU `index`; return false for indices >= 4.
    /// Allocation-free mailbox write ordered after scheduler publication.
    fn send_ipi(index: usize) -> bool {
        if index >= 4 {
            return false;
        }
        // SAFETY: orders normal scheduler stores before the Device mailbox write.
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("dsb ishst", options(nostack, preserves_flags));
        }
        write(LOCAL + 0x80 + index * 16, 1);
        true
    }

    /// Pi needs no EOI token; mailbox acknowledgement occurs before dispatch.
    type Token = ();

    /// Read current-CPU pending bits and select at most one checkpoint.
    ///
    /// Returns UART plus timer or mailbox events. Timer wins over a
    /// simultaneous mailbox so its scheduler work remains pending for the
    /// next IRQ. This IRQ-only path does not allocate or block.
    fn acknowledge() -> Option<(Self::Token, IrqEvents)> {
        let cpu = crate::arch_current_cpu_hardware_id() as usize;
        assert!(cpu < 4, "invalid BCM2837 IRQ CPU");
        let pending = read(LOCAL + 0x60 + cpu * 4);
        let checkpoint = if pending & TIMER_PENDING != 0 {
            Some(Checkpoint::Timer)
        } else if pending & MAILBOX_PENDING != 0 {
            // Clear before scheduler work so a newly published request survives.
            write(LOCAL + 0xc0 + cpu * 16, 1);
            Some(Checkpoint::SchedulerIpi)
        } else {
            None
        };
        Some((
            (),
            IrqEvents {
                uart: cpu == 0 && pending & GPU_PENDING != 0,
                checkpoint,
                unexpected: None,
            },
        ))
    }

    /// Complete Pi MMIO acknowledgement ordering after common dispatch.
    /// `token` is unit because this controller has no IAR/EOI protocol.
    /// Returns nothing and touches only local barrier state.
    fn end(_token: Self::Token) {
        // SAFETY: finish device acknowledgements before returning to the saved frame.
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
        }
    }
}
