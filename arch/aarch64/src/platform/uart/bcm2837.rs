//! Raspberry Pi 3B+ runtime console, inheriting a working U-Boot UART.
//!
//! The board DT translates BCM2837 bus addresses to ARM physical MMIO bases.
//! GPIO14/15 ALT0 selects PL011; ALT5 selects the auxiliary mini UART.
//! Preserve firmware clocks, divisors, pinmux, and line settings: mini UART's
//! baud rate depends on the VPU clock, not the PL011 reference clock.
//! Register definitions: BCM2835 ARM Peripherals, chapters 2, 6, and 13;
//! BCM2837 address translation: U-Boot arch/arm/dts/bcm2837.dtsi.

#[inline(always)]
unsafe fn mmio_read32(pa: usize) -> u32 {
    #[cfg(not(test))]
    let pa = crate::mmu::phys_to_hva(pa);
    // SAFETY: caller selects an existing mapped BCM2837 register.
    unsafe { crate::mmio::mmio_read32(pa) }
}
#[inline(always)]
unsafe fn mmio_write32(pa: usize, value: u32) {
    #[cfg(not(test))]
    let pa = crate::mmu::phys_to_hva(pa);
    // SAFETY: caller owns this register through the Device alias.
    unsafe { crate::mmio::mmio_write32(pa, value) }
}

use super::pl011::Device as Pl011;

const GPFSEL1: usize = 0x04;
const PIN_FUNCTION_MASK: u32 = 7;
const GPIO14_SHIFT: u32 = 12;
const GPIO15_SHIFT: u32 = 15;
const ALT0: u32 = 4;
const ALT5: u32 = 2;

const AUX_ENABLES: usize = 0x04;
const MINI_IO: usize = 0x00;
const MINI_LCR: usize = 0x0c;
const MINI_LSR: usize = 0x14;
const MINI_CNTL: usize = 0x20;
const MINI_TX_ENABLE: u32 = 1 << 1;
const MINI_CTS_FLOW: u32 = 1 << 3;
const MINI_DLAB: u32 = 1 << 7;
const MINI_TX_SPACE: u32 = 1 << 5;
const TX_POLL_LIMIT: usize = 100_000;
#[cfg(test)]
const MINI_TX_IDLE: u32 = 1 << 6;

/// CPU physical bases resolved from the board's translated DT `reg` entries.
#[derive(Copy, Clone)]
pub(crate) struct UartMmio {
    pub gpio: usize,
    pub pl011: usize,
    pub aux: usize,
    pub mini: usize,
}

/// The already-configured transmitter routed to the board's GPIO14/15 header.
pub(super) enum Port {
    Pl011(usize),
    Mini(usize),
}

/// Adopt the UART configured by U-Boot without changing clocks or pinmux.
///
/// # Arguments
/// * `mmio` - Validated DT-derived physical bases for GPIO and both UARTs.
///
/// # Returns
/// `Some(port)` for an enabled 8N1 transmitter on GPIO14/15 without CTS flow
/// control; `None` for unsupported pinmux or UART state. Baud rate is inherited,
/// not measured. No allocation, writes, blocking, or IRQ changes occur.
///
/// # Safety
/// Only the boot CPU may call this with Device mappings installed and IRQ masked.
/// Firmware must have finished serial transfers and must keep UART clocks stable.
pub(super) unsafe fn initialize(mmio: &UartMmio) -> Option<Port> {
    // SAFETY: the translated DT extents are mapped as Device memory. Read
    // auxiliary registers only after checking that firmware enabled AUX.
    unsafe {
        let pins = mmio_read32(mmio.gpio + GPFSEL1);
        let tx = (pins >> GPIO14_SHIFT) & PIN_FUNCTION_MASK;
        let rx = (pins >> GPIO15_SHIFT) & PIN_FUNCTION_MASK;
        if tx == ALT0 && rx == ALT0 {
            let uart = Pl011::new(crate::mmu::phys_to_hva(mmio.pl011));
            if uart.adopted_8n1() {
                return Some(Port::Pl011(mmio.pl011));
            }
        } else if tx == ALT5 && rx == ALT5 && mmio_read32(mmio.aux + AUX_ENABLES) & 1 != 0 {
            let control = mmio_read32(mmio.mini + MINI_CNTL);
            let line = mmio_read32(mmio.mini + MINI_LCR);
            // U-Boot writes 3 for 8-bit mode; checking both low bits also
            // accounts for the BCM2835 mini-UART data-size documentation erratum.
            if control & (MINI_TX_ENABLE | MINI_CTS_FLOW) == MINI_TX_ENABLE
                && line & (MINI_DLAB | 3) == 3
            {
                return Some(Port::Mini(mmio.mini));
            }
        }
    }
    None
}

impl Port {
    /// Poll and send a raw byte, without allocation or newline translation.
    ///
    /// # Arguments
    /// * `self` - Exclusive port adopted from U-Boot.
    /// * `byte` - Byte to transmit at the inherited baud rate.
    ///
    /// # Returns
    /// Nothing; drops the byte after `TX_POLL_LIMIT` unsuccessful register reads.
    ///
    /// # Safety
    /// Caller must retain the physical-MMIO and clock contract of `initialize`.
    pub(super) unsafe fn putc(&self, byte: u8) {
        // SAFETY: initialize validated pinmux and transmitter state.
        unsafe {
            match self {
                Self::Pl011(base) => {
                    let uart = Pl011::new(crate::mmu::phys_to_hva(*base));
                    let _ = uart.try_putc(byte, TX_POLL_LIMIT);
                }
                Self::Mini(base) => {
                    for _ in 0..TX_POLL_LIMIT {
                        if mmio_read32(*base + MINI_LSR) & MINI_TX_SPACE != 0 {
                            mmio_write32(*base + MINI_IO, u32::from(byte));
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Wait until the last byte leaves the selected UART.
    ///
    /// # Arguments
    /// * `self` - Exclusive port adopted from U-Boot.
    ///
    /// # Returns
    /// Nothing; polls without allocation and may wait indefinitely on failure.
    ///
    /// # Safety
    /// Caller must retain the physical-MMIO and clock contract of `initialize`.
    #[cfg(test)]
    pub(super) unsafe fn drain(&self) {
        // SAFETY: the adopted port owns this physical transmitter.
        unsafe {
            match self {
                Self::Pl011(base) => {
                    Pl011::new(crate::mmu::phys_to_hva(*base)).drain_tx();
                }
                Self::Mini(base) => while mmio_read32(*base + MINI_LSR) & MINI_TX_IDLE == 0 {},
            }
        }
    }
}

pub(crate) mod runtime {
    use super::*;
    use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

    static SELECTED: AtomicU8 = AtomicU8::new(0);
    static BASE: AtomicUsize = AtomicUsize::new(0);

    fn port() -> Option<Port> {
        let kind = SELECTED.load(Ordering::Acquire);
        let base = BASE.load(Ordering::Relaxed);
        match kind {
            1 => Some(Port::Pl011(base)),
            2 => Some(Port::Mini(base)),
            _ => None,
        }
    }

    /// Adopt the firmware UART from DT-derived physical register ranges.
    ///
    /// # Arguments
    /// * `platform` - Published early SoC MMIO mapping.
    /// * `dtb` - Board DTB with GPIO, PL011, AUX and mini-UART reg entries.
    ///
    /// # Returns
    /// Nothing; invalid DT resources or unsupported firmware UART state panic.
    /// Boot-only, allocation-free, with IRQ masked.
    #[cfg(not(test))]
    pub(crate) fn configure_from_platform(platform: &crate::platform::PlatformInfo, dtb: &[u8]) {
        let mmio =
            super::super::bcm2837_dtb::parse(dtb, platform).expect("invalid Pi UART DT resources");
        configure_from_ranges(mmio);
    }

    /// Publish a validated UART selection; used by host tests with mock MMIO.
    /// Takes DT-derived `mmio`; returns nothing and allocates nothing.
    pub(crate) fn configure_from_ranges(mmio: UartMmio) {
        // SAFETY: CPU0 owns the mapped UART with IRQ masked after firmware handoff.
        let port = unsafe { initialize(&mmio) }.expect("unsupported Pi firmware UART");
        let (kind, base) = match port {
            Port::Pl011(base) => (1, base),
            Port::Mini(base) => (2, base),
        };
        BASE.store(base, Ordering::Relaxed);
        SELECTED.store(kind, Ordering::Release);
    }

    /// Keep firmware configuration intact; no arguments or return value.
    /// Configuration is completed during boot; this function never blocks.
    pub(crate) fn init_once() {}

    /// Transmit raw `byte` through the selected UART with bounded polling.
    /// Returns nothing, drops bytes on timeout/unconfigured state; no allocation.
    pub(crate) fn putc_raw(byte: u8) {
        if let Some(port) = port() {
            // SAFETY: immutable boot selection; kernel serializes output.
            unsafe {
                port.putc(byte);
            }
        }
    }

    /// Return the GPU interrupt number for the selected UART (29 AUX, 57 PL011).
    /// No arguments; panics before configuration. Allocation-free and nonblocking.
    pub(crate) fn gpu_irq() -> u32 {
        match port().expect("Pi UART not configured") {
            Port::Pl011(_) => 57,
            Port::Mini(_) => 29,
        }
    }

    /// Enable receive interrupts for the configured UART, preserving baud/pinmux.
    /// No arguments or return value; boot-only with IRQ masked, no allocation.
    pub(crate) fn enable_rx_interrupts() {
        // SAFETY: boot CPU exclusively owns UART configuration before IRQ enable.
        unsafe {
            match port().expect("Pi UART not configured") {
                Port::Pl011(base) => {
                    let uart = Pl011::new(crate::mmu::phys_to_hva(base));
                    uart.enable_receiver();
                    uart.enable_rx_interrupts();
                }
                Port::Mini(base) => {
                    mmio_write32(base + MINI_CNTL, mmio_read32(base + MINI_CNTL) | 1);
                    mmio_write32(base + MINI_IO + 4, 1); // RX only.
                }
            }
        }
    }

    /// Drain at most 64 RX bytes into kernel stdin and acknowledge the UART.
    /// No arguments or return value; IRQ-only, bounded and allocation-free.
    pub(crate) fn on_uart_irq() {
        let Some(port) = port() else {
            return;
        };
        // SAFETY: CPU0 owns RX; all addresses are mapped Device memory.
        unsafe {
            match port {
                Port::Pl011(base) => {
                    Pl011::new(crate::mmu::phys_to_hva(base)).drain_rx(64);
                }
                Port::Mini(base) => {
                    for _ in 0..64 {
                        if mmio_read32(base + MINI_LSR) & 1 == 0 {
                            break;
                        }
                        kernel::console::on_stdin_byte(mmio_read32(base + MINI_IO) as u8);
                    }
                }
            }
        }
    }
}
