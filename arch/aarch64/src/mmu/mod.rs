//! AArch64 MMU bring-up для QEMU `virt`.
//!
//! Модуль намеренно разделен по ownership:
//! - `boot`: low `.boot.*` state и builder, исполняемый до включения MMU;
//! - `hva`: high direct-map/HVA helpers и post-init TTBR1 page-table операции;
//! - `abi`: C ABI surface для arch-agnostic `kernel/` кода.

#![allow(dead_code)]

mod abi;
mod boot;
mod hva;

pub use self::{
    boot::init_from_boot_params,
    hva::{phys_to_hva, phys_to_hva_const},
};

pub(crate) use self::boot::platform_params_from_boot_params;
