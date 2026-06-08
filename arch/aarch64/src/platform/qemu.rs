//! QEMU `virt` bare-metal boot protocol constants.
//!
//! `xtask` loads the compacted QEMU-generated DTB at the start of RAM with
//! `-device loader,...,addr=0x40000000`. The low `.boot.text` MMU builder reads
//! that slot before UART/GIC are initialized and before high Rust can parse the
//! DTB with normal helpers.

use super::{BootDeviceRange, BootPlatformInfo};

/// Physical address where the QEMU `virt` DTB is loaded by the current xtask
/// bare-metal ELF flow.
pub(crate) const BOOT_DTB_PA: usize = 0x4000_0000;
pub(crate) const USER_IMAGE_LOAD_PA: usize = 0x4700_0000;
pub(crate) const USER_IMAGE_RESERVED_SIZE: usize = 64 * 1024;

const RAM_START: usize = 0x4000_0000;
const RAM_SIZE: usize = 0x0800_0000;
const UART_START: usize = 0x0900_0000;
const GICD_START: usize = 0x0800_0000;
const GICC_START: usize = 0x0801_0000;

// Emergency fallback window, not an authoritative device `reg` size. The real
// device ranges are expected to come from the generated QEMU DTB; this only
// keeps early UART/GIC MMIO reachable when that parse fails before logging.
const EMERGENCY_MMIO_SIZE: usize = 0x1000;

#[unsafe(link_section = ".boot.text")]
pub(crate) fn apply_fallback_platform_info(info: &mut BootPlatformInfo) {
    let dtb_pa = info.dtb_pa;
    let dtb_size = info.dtb_size;

    info.dtb_pa = dtb_pa;
    info.dtb_size = dtb_size;
    info.ram = BootDeviceRange {
        start: RAM_START,
        size: RAM_SIZE,
    };
    info.uart = BootDeviceRange {
        start: UART_START,
        size: EMERGENCY_MMIO_SIZE,
    };
    info.gic_distributor = BootDeviceRange {
        start: GICD_START,
        size: EMERGENCY_MMIO_SIZE,
    };
    info.gic_cpu_interface = BootDeviceRange {
        start: GICC_START,
        size: EMERGENCY_MMIO_SIZE,
    };
}
