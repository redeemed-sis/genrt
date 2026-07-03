use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{
    mmio::{mmio_read32, mmio_write32},
    mmu::phys_to_hva_const,
    platform::PlatformInfo,
};

const FR_TXFF: u32 = 1 << 5;
const FR_RXFE: u32 = 1 << 4;
const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;
const LCR_H_FEN: u32 = 1 << 4;
const LCR_H_WLEN_8: u32 = 0b11 << 5;
const IMSC_RXIM: u32 = 1 << 4;
const IMSC_RTIM: u32 = 1 << 6;
const ICR_RXIC: u32 = 1 << 4;
const ICR_RTIC: u32 = 1 << 6;
const MAX_TX_SPINS: usize = 4096;

static UART_INIT_DONE: AtomicBool = AtomicBool::new(false);
static PL011_BASE: AtomicUsize = AtomicUsize::new(0);

pub fn configure_from_platform(platform: &PlatformInfo) {
    if platform.uart.is_present() {
        PL011_BASE.store(
            phys_to_hva_const(platform.uart.start as usize),
            Ordering::Relaxed,
        );
        UART_INIT_DONE.store(false, Ordering::Relaxed);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_console_init_once() {
    if UART_INIT_DONE.load(Ordering::Relaxed) {
        return;
    }
    let base = PL011_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return;
    }

    // SAFETY: `base` came from the parsed DTB PL011 `reg` property and points
    // at the high direct-map alias of the UART MMIO range.
    unsafe {
        mmio_write32(base + 0x30, 0);
        mmio_write32(base + 0x44, 0x7ff);
        mmio_write32(base + 0x38, 0);
        mmio_write32(base + 0x24, 13);
        mmio_write32(base + 0x28, 1);
        mmio_write32(base + 0x2c, LCR_H_FEN | LCR_H_WLEN_8);
        mmio_write32(base + 0x30, CR_UARTEN | CR_TXE | CR_RXE);
    }

    UART_INIT_DONE.store(true, Ordering::Relaxed);
}

pub fn enable_rx_interrupts() {
    arch_console_init_once();

    let base = PL011_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return;
    }

    // SAFETY: `base` came from the parsed DTB PL011 `reg` property.
    unsafe {
        mmio_write32(base + 0x44, ICR_RXIC | ICR_RTIC);
        mmio_write32(base + 0x38, IMSC_RXIM | IMSC_RTIM);
    }
}

pub fn on_uart_irq() {
    let base = PL011_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return;
    }

    // SAFETY: UART MMIO base is initialized from the platform PL011 range. IRQ
    // handler drains RX FIFO without allocation and hands raw bytes to kernel
    // stdin buffering; terminal line discipline lives in userspace.
    unsafe {
        while (mmio_read32(base + 0x18) & FR_RXFE) == 0 {
            let byte = (mmio_read32(base) & 0xff) as u8;
            kernel::console::on_stdin_byte(byte);
        }
        mmio_write32(base + 0x44, ICR_RXIC | ICR_RTIC);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_console_putc_raw(c: u8) {
    let base = PL011_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return;
    }

    // SAFETY: `base` came from the parsed DTB PL011 `reg` property.
    unsafe {
        let mut spins = MAX_TX_SPINS;

        while spins != 0 {
            let fr = mmio_read32(base + 0x18);
            if (fr & FR_TXFF) == 0 {
                mmio_write32(base, c as u32);
                return;
            }
            spins -= 1;
        }

        // Early bring-up fallback:
        // if FR polling is unreliable, still attempt the write once.
        mmio_write32(base, c as u32);
    }
}
