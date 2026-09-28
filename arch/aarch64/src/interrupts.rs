//! Static contract for the selected platform interrupt controller.
//!
//! GICv2 uses IAR/EOIR; BCM2837 uses local pending bits and mailbox clears.
//! Only shared operations cross this boundary, with no IRQ-time vtable lookup.

use crate::{
    console,
    platform::{self, PlatformInfo},
    timer,
};

type Selected = platform::interrupt_controller::Controller;

/// At most one scheduler checkpoint is permitted per IRQ return.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Checkpoint {
    Timer,
    SchedulerIpi,
}

/// Sources reported by one controller acknowledgement.
///
/// UART may accompany one timer or IPI on the Pi cascade. A GIC IAR reports
/// only one source. The controller retains its own acknowledgement token.
#[derive(Default)]
pub(crate) struct IrqEvents {
    pub(crate) uart: bool,
    pub(crate) checkpoint: Option<Checkpoint>,
    pub(crate) unexpected: Option<u32>,
}

/// Controller operations required by architecture entry and boot.
pub(crate) trait InterruptController {
    /// Initialize CPU0-owned routing from validated `platform` MMIO ranges.
    /// Returns nothing; failure is fatal to boot.
    fn init_global(platform: &PlatformInfo);

    /// Initialize the executing CPU's private sources. Takes no arguments;
    /// returns whether timer and scheduler notification are ready.
    fn init_local() -> bool;

    /// Bind the executing interface to logical CPU `index`. Returns whether
    /// that CPU can receive targeted notifications.
    fn bind_cpu(index: usize) -> bool;

    /// Notify logical CPU `index` of scheduler work. Returns whether the
    /// target was valid and notification was sent.
    fn send_ipi(index: usize) -> bool;

    /// Controller-owned state required to complete one acknowledged IRQ.
    type Token;

    /// Read local pending state and acknowledge sources as required.
    /// Takes no arguments. Returns `None` for a spurious IRQ, otherwise
    /// the controller token and bounded events for common dispatch.
    fn acknowledge() -> Option<(Self::Token, IrqEvents)>;

    /// Complete controller-specific IRQ ordering using `token`.
    /// Returns nothing; QEMU EOIs the full IAR and Pi completes its MMIO
    /// acknowledgement barrier after common source handling.
    fn end(token: Self::Token);
}

/// Initialize CPU0-owned routing from validated `platform` ranges.
/// Returns nothing; the selected controller reports fatal boot failures.
pub(crate) fn init_global(platform: &PlatformInfo) {
    <Selected as InterruptController>::init_global(platform);
}

/// Initialize the executing CPU's private interrupt sources.
/// Takes no arguments; returns whether they are ready.
pub(crate) fn init_local() -> bool {
    <Selected as InterruptController>::init_local()
}

/// Bind executing controller state to logical CPU `index`.
/// Returns whether the target is valid and available.
pub(crate) fn bind_cpu(index: usize) -> bool {
    <Selected as InterruptController>::bind_cpu(index)
}

/// Notify logical CPU `index` of scheduler work.
/// Returns whether the target is bound and notification was sent.
pub(crate) fn send_ipi(index: usize) -> bool {
    <Selected as InterruptController>::send_ipi(index)
}

/// Dispatch one IRQ with exclusive live return frame `active`.
/// Returns after source handling and acknowledgement without allocation.
pub(crate) fn dispatch(active: &mut kernel::arch::ActiveContext<'_>) {
    let Some((token, events)) = <Selected as InterruptController>::acknowledge() else {
        return;
    };
    if events.uart {
        console::on_uart_irq();
    }
    match events.checkpoint {
        Some(Checkpoint::Timer) => timer::on_timer_irq(active),
        Some(Checkpoint::SchedulerIpi) => kernel::sched::finish_scheduler_ipi(active),
        None => {}
    }
    if let Some(id) = events.unexpected {
        kernel::warn!("irq: unexpected id=0x{id:08x}");
    }
    <Selected as InterruptController>::end(token);
    #[cfg(feature = "qemu-test-smp-boot")]
    if events.checkpoint == Some(Checkpoint::Timer) {
        kernel::time::on_local_timer_probe_eoi_for_test();
    }
}
