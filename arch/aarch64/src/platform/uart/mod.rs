//! Platform UART backends for QEMU and Raspberry Pi runtime.
//!
//! PL011 serves QEMU; BCM2837 adopts the firmware UART for Pi runtime TX/RX.

#[cfg(feature = "platform-raspberrypi-3b-plus")]
pub(crate) mod bcm2837;
#[cfg(feature = "platform-raspberrypi-3b-plus")]
mod bcm2837_dtb;
#[cfg(feature = "platform-raspberrypi-3b-plus")]
pub(crate) use bcm2837::runtime;
pub(crate) mod pl011;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
pub(crate) use pl011 as runtime;
