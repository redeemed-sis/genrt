//! QEMU virt GICv2 interrupt routing and dispatch.

use crate::{
    console,
    interrupts::{Checkpoint, InterruptController, IrqEvents},
    platform,
};

use super::gic;

/// GICv2 controller selected for QEMU virt boot.
pub(crate) struct Controller;

const PHYSICAL_TIMER_PPI: u32 = 30;

impl InterruptController for Controller {
    /// Initialize shared GIC/UART routing with IRQ masked on CPU0.
    ///
    /// # Arguments
    ///
    /// * `platform` - Published device ranges containing the GIC interfaces.
    ///
    /// # Returns
    ///
    /// Returns nothing. Boot-only and allocation-free.
    ///
    /// # Panics
    ///
    /// Panics if GIC distributor or UART SPI routing initialization fails.
    fn init_global(platform: &platform::PlatformInfo) {
        gic::configure_from_platform(platform);
        assert!(
            gic::init_global_distributor(),
            "GIC distributor init failed"
        );
        console::enable_rx_interrupts();
        assert!(gic::enable_spi_on_cpu0(platform::qemu::UART0_IRQ_ID, 0x60));
    }

    /// Enable the executing CPU's GIC interface, timer and SGI with IRQ masked.
    ///
    /// # Returns
    ///
    /// Returns `true` after all local register readbacks succeed; otherwise
    /// returns `false`. Takes no arguments, allocates nothing and does not block.
    fn init_local() -> bool {
        gic::init_current_cpu_interface()
            && gic::enable_current_cpu_private_irq(PHYSICAL_TIMER_PPI, 0x40)
            && gic::init_current_cpu_scheduler_ipi()
    }

    /// Bind a logical CPU to the executing GIC target.
    ///
    /// # Arguments
    ///
    /// * `index` - Logical CPU slot for the executing interface.
    ///
    /// # Returns
    ///
    /// Returns `true` when the target binding succeeds; otherwise `false`.
    /// This operation is bounded and allocation-free.
    fn bind_cpu(index: usize) -> bool {
        gic::bind_current_cpu_target(index)
    }

    /// Send a scheduler IPI to one logical CPU.
    ///
    /// # Arguments
    ///
    /// * `index` - Logical destination CPU.
    ///
    /// # Returns
    ///
    /// Returns `true` after a targeted SGI write, or `false` for an unbound
    /// destination. This operation is bounded and allocation-free.
    fn send_ipi(index: usize) -> bool {
        gic::send_scheduler_ipi(index)
    }

    /// Acknowledge the executing CPU's full GIC IAR value and classify its
    /// source. A spurious IAR has no EOI token.
    ///
    /// # Returns
    ///
    /// Returns the full IAR and one event, or `None` for a spurious IRQ.
    /// This IRQ-only operation does not allocate or block.
    type Token = u32;

    fn acknowledge() -> Option<(Self::Token, IrqEvents)> {
        let iar = gic::acknowledge_irq();
        let id = gic::irq_id_from_iar(iar);
        if gic::is_spurious(id) {
            return None;
        }
        let mut events = IrqEvents::default();
        if id == PHYSICAL_TIMER_PPI {
            events.checkpoint = Some(Checkpoint::Timer);
        } else if id == gic::SCHEDULER_IPI_IRQ_ID {
            #[cfg(feature = "qemu-test-smp-boot")]
            {
                let encoded = crate::arch_current_cpu_logical_id();
                assert!(encoded != 0, "scheduler IPI reached an unbound CPU");
                gic::record_scheduler_ipi_for_test(encoded - 1);
            }
            events.checkpoint = Some(Checkpoint::SchedulerIpi);
        } else if id == platform::qemu::UART0_IRQ_ID {
            events.uart = true;
        } else {
            events.unexpected = Some(id);
        }
        Some((iar, events))
    }

    /// EOI the exact `token` returned by GICC_IAR after common dispatch.
    /// Returns nothing; the GIC register protocol stays platform-owned.
    fn end(token: Self::Token) {
        gic::end_irq(token);
    }
}
