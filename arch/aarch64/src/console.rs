//! Runtime console ABI facade; register access belongs to platform UARTs.
//!
//! QEMU uses its PL011 backend; Raspberry Pi adopts the firmware-selected
//! PL011 or mini UART.

use crate::platform::uart::runtime as backend;

pub(crate) use backend::{configure_from_platform, enable_rx_interrupts, on_uart_irq};

/// Initialize the runtime console through the platform UART backend.
///
/// # Arguments
/// None; boot must first publish the platform UART configuration.
///
/// # Returns
/// Returns nothing; an unconfigured UART is a no-op. Concurrent initialization
/// may spin until the initializing CPU completes. Allocates nothing.
#[unsafe(no_mangle)]
pub extern "C" fn arch_console_init_once() {
    backend::init_once();
}

/// Send a raw byte through the runtime console backend.
///
/// # Arguments
/// * `c` - Byte to transmit. Caller owns output serialization.
///
/// # Returns
/// Returns nothing; an unconfigured UART drops the byte. Polling is bounded,
/// with no allocation, scheduler handoff, or newline translation.
#[unsafe(no_mangle)]
pub extern "C" fn arch_console_putc_raw(c: u8) {
    backend::putc_raw(c);
}
