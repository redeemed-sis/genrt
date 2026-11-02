//! Bootstrap discovery of the firmware-selected BCM2837 UART resources.

use crate::platform::{PlatformInfo, uart::bcm2837::UartMmio};
use kernel::dtb::RuntimeDtb;

/// Resolve GPIO and both UART register sets through shared bus translation.
/// Disabled UART nodes are intentionally included: firmware selected the live
/// transmitter and the driver adopts it without changing clocks or pinmux.
/// # Arguments
/// * `tree` - Shared validated runtime DTB reader.
/// * `platform` - Already mapped early peripheral window.
/// # Returns
/// All required, unique register bases contained in that mapped window, or None
/// for missing/invalid resources. Allocation-free and CPU0 boot-only.
pub(super) fn parse(tree: &RuntimeDtb<'_>, platform: &PlatformInfo) -> Option<UartMmio> {
    let (mut gpio, mut pl011, mut aux, mut mini) = (None, None, None, None);
    for node in tree.fdt().all_nodes() {
        let kind = if node.compatibles().any(|name| name == "brcm,bcm2835-gpio") {
            0
        } else if node.compatibles().any(|name| name == "arm,pl011") {
            1
        } else if node.compatibles().any(|name| name == "brcm,bcm2835-aux") {
            2
        } else if node
            .compatibles()
            .any(|name| name == "brcm,bcm2835-aux-uart")
        {
            3
        } else {
            continue;
        };
        let range = tree.mmio_reg(&node, 0).ok()?;
        let end = range.start.checked_add(range.size)?;
        if range.size < [8, 0x48, 8, 0x24][kind]
            || range.start < platform.uart.start
            || end > platform.uart.start.checked_add(platform.uart.size)?
        {
            return None;
        }
        let base = usize::try_from(range.start).ok()?;
        let target = match kind {
            0 => &mut gpio,
            1 => &mut pl011,
            2 => &mut aux,
            _ => &mut mini,
        };
        if target.replace(base).is_some() {
            return None;
        }
    }
    Some(UartMmio {
        gpio: gpio?,
        pl011: pl011?,
        aux: aux?,
        mini: mini?,
    })
}
