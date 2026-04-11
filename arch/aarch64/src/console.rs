use core::sync::atomic::{AtomicBool, Ordering};

use crate::mmio::{mmio_read32, mmio_write32};

const PL011_BASE: usize = 0x0900_0000;
const UARTDR_ADDR: usize = PL011_BASE;
const UARTFR_ADDR: usize = PL011_BASE + 0x18;
const UARTIBRD_ADDR: usize = PL011_BASE + 0x24;
const UARTFBRD_ADDR: usize = PL011_BASE + 0x28;
const UARTLCR_H_ADDR: usize = PL011_BASE + 0x2c;
const UARTCR_ADDR: usize = PL011_BASE + 0x30;
const UARTICR_ADDR: usize = PL011_BASE + 0x44;

const FR_TXFF: u32 = 1 << 5;
const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;
const LCR_H_FEN: u32 = 1 << 4;
const LCR_H_WLEN_8: u32 = 0b11 << 5;
const MAX_TX_SPINS: usize = 4096;

static UART_INIT_DONE: AtomicBool = AtomicBool::new(false);

#[unsafe(no_mangle)]
pub extern "C" fn arch_console_init_once() {
    if UART_INIT_DONE.load(Ordering::Relaxed) {
        return;
    }

    // SAFETY: Early boot, single-core bring-up path on QEMU virt PL011.
    unsafe {
        mmio_write32(UARTCR_ADDR, 0);
        mmio_write32(UARTICR_ADDR, 0x7ff);
        mmio_write32(UARTIBRD_ADDR, 13);
        mmio_write32(UARTFBRD_ADDR, 1);
        mmio_write32(UARTLCR_H_ADDR, LCR_H_FEN | LCR_H_WLEN_8);
        mmio_write32(UARTCR_ADDR, CR_UARTEN | CR_TXE | CR_RXE);
    }

    UART_INIT_DONE.store(true, Ordering::Relaxed);
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_console_putc_raw(c: u8) {
    // SAFETY: Fixed MMIO UART registers on QEMU virt.
    unsafe {
        let mut spins = MAX_TX_SPINS;

        while spins != 0 {
            let fr = mmio_read32(UARTFR_ADDR);
            if (fr & FR_TXFF) == 0 {
                mmio_write32(UARTDR_ADDR, c as u32);
                return;
            }
            spins -= 1;
        }

        // Early bring-up fallback:
        // if FR polling is unreliable, still attempt the write once.
        mmio_write32(UARTDR_ADDR, c as u32);
    }
}
