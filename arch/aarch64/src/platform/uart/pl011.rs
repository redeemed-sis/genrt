//! Shared PL011 register operations and the QEMU runtime UART adapter.

use crate::mmio::{mmio_read32, mmio_write32};

const FR_TXFF: u32 = 1 << 5;
const FR_RXFE: u32 = 1 << 4;
const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
const LCR_H_FEN: u32 = 1 << 4;
const LCR_H_WLEN_8: u32 = 0b11 << 5;
const IMSC_RXIM: u32 = 1 << 4;
const IMSC_RTIM: u32 = 1 << 6;
const ICR_RXIC: u32 = 1 << 4;
const ICR_RTIC: u32 = 1 << 6;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
const MAX_TX_SPINS: usize = 4096;

const DR: usize = 0x00;
const FR: usize = 0x18;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
const IBRD: usize = 0x24;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
const FBRD: usize = 0x28;
const LCR_H: usize = 0x2c;
const CR: usize = 0x30;
const IMSC: usize = 0x38;
const ICR: usize = 0x44;
#[cfg(test)]
const FR_BUSY: u32 = 1 << 3;
#[cfg(any(test, feature = "platform-raspberrypi-3b-plus"))]
const CR_CTSEN: u32 = 1 << 15;
#[cfg(any(test, feature = "platform-raspberrypi-3b-plus"))]
const LCR_H_BREAK_PARITY_TWO_STOP: u32 = 1 | (1 << 1) | (1 << 3);

/// Shared PL011 register operations for DT-selected QEMU and Pi UARTs.
#[derive(Copy, Clone)]
pub(super) struct Device {
    base: usize,
}

impl Device {
    pub(super) const fn new(base: usize) -> Self {
        Self { base }
    }

    /// Check the firmware transmitter without changing its baud or pinmux.
    /// Returns true only for enabled 8N1 TX without CTS flow control.
    #[cfg(any(test, feature = "platform-raspberrypi-3b-plus"))]
    pub(super) unsafe fn adopted_8n1(self) -> bool {
        // SAFETY: caller supplies a mapped PL011 Device MMIO base.
        unsafe {
            let control = mmio_read32(self.base + CR);
            let line = mmio_read32(self.base + LCR_H);
            control & (CR_UARTEN | CR_TXE | CR_CTSEN) == CR_UARTEN | CR_TXE
                && line & (LCR_H_WLEN_8 | LCR_H_BREAK_PARITY_TWO_STOP) == LCR_H_WLEN_8
        }
    }

    /// Program the QEMU UART; Pi intentionally never calls this.
    /// No arguments or return value beyond the mapped device base.
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    pub(super) unsafe fn initialize_8n1(self) {
        // SAFETY: caller exclusively owns this mapped PL011 during boot.
        unsafe {
            mmio_write32(self.base + CR, 0);
            mmio_write32(self.base + ICR, 0x7ff);
            mmio_write32(self.base + IMSC, 0);
            mmio_write32(self.base + IBRD, 13);
            mmio_write32(self.base + FBRD, 1);
            mmio_write32(self.base + LCR_H, LCR_H_FEN | LCR_H_WLEN_8);
            mmio_write32(self.base + CR, CR_UARTEN | CR_TXE | CR_RXE);
        }
    }

    /// Enable the receiver on a firmware-configured PL011; preserve all
    /// other control bits. No return value; boot-only with mapped Device MMIO.
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    pub(super) unsafe fn enable_receiver(self) {
        // SAFETY: caller exclusively owns this mapped PL011 configuration.
        unsafe {
            mmio_write32(self.base + CR, mmio_read32(self.base + CR) | CR_RXE);
        }
    }

    /// Enable and clear RX/timeout interrupts without changing line settings.
    /// No arguments or return value beyond the mapped device base.
    pub(super) unsafe fn enable_rx_interrupts(self) {
        // SAFETY: caller owns this mapped PL011 interrupt configuration.
        unsafe {
            mmio_write32(self.base + ICR, ICR_RXIC | ICR_RTIC);
            mmio_write32(self.base + IMSC, IMSC_RXIM | IMSC_RTIM);
        }
    }

    /// Poll TX for at most `limit` reads; return whether the byte was sent.
    /// Caller may choose its own timeout policy.
    pub(super) unsafe fn try_putc(self, byte: u8, limit: usize) -> bool {
        // SAFETY: caller supplies a mapped PL011 Device MMIO base.
        unsafe {
            for _ in 0..limit {
                if mmio_read32(self.base + FR) & FR_TXFF == 0 {
                    mmio_write32(self.base + DR, u32::from(byte));
                    return true;
                }
            }
        }
        false
    }

    /// Write a byte without polling; used only by QEMU's existing fallback.
    /// No return value; caller owns the mapped transmitter.
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    pub(super) unsafe fn write_data(self, byte: u8) {
        // SAFETY: caller supplies a mapped PL011 Device MMIO base.
        unsafe { mmio_write32(self.base + DR, u32::from(byte)) };
    }

    /// Drain at most `limit` bytes to stdin and acknowledge RX sources.
    /// Returns nothing; the IRQ path does not allocate.
    pub(super) unsafe fn drain_rx(self, limit: usize) {
        // SAFETY: caller owns the mapped PL011 IRQ and stdin handoff.
        unsafe {
            for _ in 0..limit {
                if mmio_read32(self.base + FR) & FR_RXFE != 0 {
                    break;
                }
                let byte = mmio_read32(self.base + DR) as u8;
                kernel::console::on_stdin_byte(byte);
            }
            mmio_write32(self.base + ICR, ICR_RXIC | ICR_RTIC);
        }
    }

    /// Wait for the adopted transmitter to become idle. Test-only.
    #[cfg(test)]
    pub(super) unsafe fn drain_tx(self) {
        // SAFETY: caller supplies a mapped PL011 Device MMIO base.
        unsafe { while mmio_read32(self.base + FR) & FR_BUSY != 0 {} }
    }
}

#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
mod qemu_runtime {
    use super::*;
    use crate::{mmu::phys_to_hva, platform::PlatformInfo};
    use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

    const UART_UNINITIALIZED: u8 = 0;
    const UART_INITIALIZING: u8 = 1;
    const UART_READY: u8 = 2;

    static UART_INIT_STATE: AtomicU8 = AtomicU8::new(UART_UNINITIALIZED);
    static PL011_BASE: AtomicUsize = AtomicUsize::new(0);

    /// Publish the UART MMIO alias before runtime console use.
    ///
    /// # Arguments
    /// * `platform` - Validated QEMU platform ranges with a mapped PL011 UART.
    ///
    /// # Returns
    /// Returns nothing; an absent UART leaves the existing configuration unchanged.
    /// Boot must call this before concurrent console use. No allocation occurs.
    pub(crate) fn configure_from_platform(platform: &PlatformInfo, _dtb: &[u8]) {
        if platform.uart.is_present() {
            PL011_BASE.store(phys_to_hva(platform.uart.start as usize), Ordering::Release);
            UART_INIT_STATE.store(UART_UNINITIALIZED, Ordering::Release);
        }
    }

    /// Initialize the published PL011 once, without allocation.
    ///
    /// # Arguments
    /// None; uses the base published by `configure_from_platform`.
    ///
    /// # Returns
    /// Returns nothing. Does nothing when unconfigured or already initialized;
    /// concurrent callers spin until the initializing CPU publishes completion.
    pub(crate) fn init_once() {
        if UART_INIT_STATE.load(Ordering::Acquire) == UART_READY {
            return;
        }
        let base = PL011_BASE.load(Ordering::Acquire);
        if base == 0 {
            return;
        }

        if UART_INIT_STATE
            .compare_exchange(
                UART_UNINITIALIZED,
                UART_INITIALIZING,
                Ordering::Acquire,
                Ordering::Acquire,
            )
            .is_err()
        {
            while UART_INIT_STATE.load(Ordering::Acquire) == UART_INITIALIZING {
                core::hint::spin_loop();
            }
            return;
        }

        // SAFETY: `base` came from the parsed DTB PL011 `reg` property and points
        // at the high direct-map alias of the UART MMIO range.
        unsafe { Device::new(base).initialize_8n1() };

        UART_INIT_STATE.store(UART_READY, Ordering::Release);
    }

    /// Enable PL011 receive and receive-timeout interrupts after initialization.
    ///
    /// # Arguments
    /// None; uses the published UART base. GIC routing is owned by the caller.
    ///
    /// # Returns
    /// Returns nothing; does nothing when unconfigured. Allocates nothing and may
    /// spin waiting for concurrent initialization, as described by `init_once`.
    pub(crate) fn enable_rx_interrupts() {
        init_once();

        let base = PL011_BASE.load(Ordering::Acquire);
        if base == 0 {
            return;
        }

        // SAFETY: `base` came from the parsed DTB PL011 `reg` property.
        unsafe { Device::new(base).enable_rx_interrupts() };
    }

    /// Drain received bytes into kernel stdin and acknowledge PL011 RX interrupts.
    ///
    /// # Arguments
    /// None; the caller must be handling this UART's IRQ after configuration.
    ///
    /// # Returns
    /// Returns nothing; does nothing when unconfigured. The IRQ path allocates
    /// nothing and reads until the receive FIFO is empty.
    pub(crate) fn on_uart_irq() {
        let base = PL011_BASE.load(Ordering::Acquire);
        if base == 0 {
            return;
        }

        // SAFETY: UART MMIO base is initialized from the platform PL011 range. IRQ
        // handler drains RX FIFO without allocation and hands raw bytes to kernel
        // stdin buffering; terminal line discipline lives in userspace.
        unsafe { Device::new(base).drain_rx(usize::MAX) };
    }

    /// Transmit one byte with bounded FIFO polling and a final fallback write.
    ///
    /// # Arguments
    /// * `c` - Raw byte, without newline translation. Caller serializes output.
    ///
    /// # Returns
    /// Returns nothing; drops the byte when unconfigured. Polls at most
    /// `MAX_TX_SPINS` times, allocates nothing, and does not yield.
    pub(crate) fn putc_raw(c: u8) {
        let base = PL011_BASE.load(Ordering::Acquire);
        if base == 0 {
            return;
        }

        // SAFETY: `base` came from the parsed DTB PL011 `reg` property.
        unsafe {
            let uart = Device::new(base);
            if !uart.try_putc(c, MAX_TX_SPINS) {
                // Preserve the QEMU bring-up fallback after bounded polling.
                uart.write_data(c);
            }
        }
    }
}

#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
pub(crate) use qemu_runtime::*;
