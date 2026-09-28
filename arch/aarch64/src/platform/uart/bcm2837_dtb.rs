//! Resolve BCM2837 UART and GPIO registers from the board DTB.
//!
//! The bundled tree describes bus addresses. The single simple-bus `ranges`
//! entry translates them to the same CPU physical window mapped before MMU
//! enable. Disabled serial nodes still describe hardware: U-Boot, not this
//! tree, selected and configured the active transmitter.

use fdt_raw::Fdt;

use crate::platform::PlatformInfo;

use super::bcm2837::UartMmio;

struct BusWindow {
    child_address: u64,
    parent_address: u64,
    length: u64,
}

fn read_cells(cells: &mut impl Iterator<Item = u32>, count: u8) -> Option<u64> {
    if !(1..=2).contains(&count) {
        return None;
    }
    let mut value = 0u64;
    for _ in 0..count {
        value = (value << 32) | u64::from(cells.next()?);
    }
    Some(value)
}

/// Parse the Pi GPIO, PL011, AUX, and mini-UART register bases.
///
/// # Arguments
///
/// * `dtb` - Resident, validated board DTB supplied by U-Boot.
/// * `platform` - Published early MMIO window derived from `soc/ranges`.
///
/// # Returns
///
/// `Some` if every required register extent is unique and contained in the
/// mapped bus window; otherwise `None`. Parsing is allocation-free and is
/// performed only by the boot CPU before UART publication.
pub(super) fn parse(dtb: &[u8], platform: &PlatformInfo) -> Option<UartMmio> {
    let fdt = Fdt::from_bytes(dtb).ok()?;
    let soc = fdt
        .all_nodes()
        .find(|node| node.compatibles().any(|name| name == "simple-bus"))?;
    let root = fdt.all_nodes().next()?;
    let mut cells = soc.find_property("ranges")?.as_u32_iter();
    // Parse each side with its own address/size cell widths. fdt_raw's
    // ranges() iterator uses the parent size width for this board's bus.
    let window = BusWindow {
        child_address: read_cells(&mut cells, soc.address_cells)?,
        parent_address: read_cells(&mut cells, root.address_cells)?,
        length: read_cells(&mut cells, soc.size_cells)?,
    };
    if cells.next().is_some()
        || window.length == 0
        || window.parent_address != platform.uart.start
        || window.length != platform.uart.size
    {
        return None;
    }
    window.child_address.checked_add(window.length)?;
    window.parent_address.checked_add(window.length)?;

    let translate = |address: u64, size: u64, minimum: u64| {
        let offset = address.checked_sub(window.child_address)?;
        let end = offset.checked_add(size)?;
        if size < minimum || end > window.length {
            return None;
        }
        usize::try_from(window.parent_address.checked_add(offset)?).ok()
    };

    let (mut gpio, mut pl011, mut aux, mut mini) = (None, None, None, None);
    for node in fdt.all_nodes() {
        if node.level() != soc.level() + 1 {
            continue;
        }
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
        let reg = node.reg()?.next()?;
        let base = translate(reg.address, reg.size?, [8, 0x48, 8, 0x24][kind])?;
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
