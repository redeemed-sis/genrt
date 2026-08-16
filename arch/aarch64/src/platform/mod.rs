//! AArch64 platform ranges discovered by the low boot DTB parser.
//!
//! The first DTB parse happens in `.boot.text`, before MMU enable, because UART
//! and GIC must already be mapped when high Rust starts logging. This module is
//! the high-side storage/validation surface for arch platform ranges; concrete
//! boot-protocol constants live in platform-specific submodules.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, Ordering},
};

mod boot_dtb;
mod cpu;
pub(crate) mod qemu;

pub(crate) use self::boot_dtb::{BootDeviceRange, BootPlatformInfo, parse_boot_platform};
pub(crate) use self::cpu::{cpu_count, expected_hardware_id, init_cpu_topology, psci_cpu_on_call};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PlatformError {
    AlreadyInitialized,
    MissingRam,
    MissingUart,
    MissingGic,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DeviceRange {
    pub start: u64,
    pub size: u64,
}

impl DeviceRange {
    pub const fn is_present(self) -> bool {
        self.start != 0 && self.size != 0
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PlatformInfo {
    pub ram: DeviceRange,
    pub uart: DeviceRange,
    pub gic_distributor: DeviceRange,
    pub gic_cpu_interface: DeviceRange,
}

impl PlatformInfo {
    fn validate(self) -> Result<Self, PlatformError> {
        if !self.ram.is_present() {
            return Err(PlatformError::MissingRam);
        }
        if !self.uart.is_present() {
            return Err(PlatformError::MissingUart);
        }
        if !self.gic_distributor.is_present() || !self.gic_cpu_interface.is_present() {
            return Err(PlatformError::MissingGic);
        }
        Ok(self)
    }
}

/// Platform-owned boot parameters discovered by the low DTB parser.
#[repr(C)]
pub(crate) struct BootPlatformParams {
    /// Physical DTB base discovered from the platform boot protocol.
    pub dtb_pa: u64,
    /// DTB total size from the FDT header.
    pub dtb_size: u64,
    pub ram_start: u64,
    pub ram_size: u64,
    pub uart_start: u64,
    pub uart_size: u64,
    pub gicd_start: u64,
    pub gicd_size: u64,
    pub gicc_start: u64,
    pub gicc_size: u64,
}

impl BootPlatformParams {
    pub(crate) const fn zeroed() -> Self {
        Self {
            dtb_pa: 0,
            dtb_size: 0,
            ram_start: 0,
            ram_size: 0,
            uart_start: 0,
            uart_size: 0,
            gicd_start: 0,
            gicd_size: 0,
            gicc_start: 0,
            gicc_size: 0,
        }
    }
}

const PLATFORM_EMPTY: u8 = 0;
const PLATFORM_INITIALIZING: u8 = 1;
const PLATFORM_READY: u8 = 2;

struct PlatformPublication {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<PlatformInfo>>,
}

// SAFETY: `init` claims the sole write with an atomic state transition and
// publishes the immutable value with Release ordering. Readers dereference it
// only after an Acquire load observes `PLATFORM_READY`.
unsafe impl Sync for PlatformPublication {}

static PLATFORM: PlatformPublication = PlatformPublication {
    state: AtomicU8::new(PLATFORM_EMPTY),
    value: UnsafeCell::new(MaybeUninit::uninit()),
};

pub unsafe fn init(platform: PlatformInfo) -> Result<&'static PlatformInfo, PlatformError> {
    let valid = platform.validate()?;
    if PLATFORM
        .state
        .compare_exchange(
            PLATFORM_EMPTY,
            PLATFORM_INITIALIZING,
            Ordering::Acquire,
            Ordering::Relaxed,
        )
        .is_err()
    {
        return Err(PlatformError::AlreadyInitialized);
    }
    // SAFETY: this initializer exclusively owns the unpublished storage.
    unsafe { (*PLATFORM.value.get()).write(valid) };
    PLATFORM.state.store(PLATFORM_READY, Ordering::Release);
    info().ok_or(PlatformError::AlreadyInitialized)
}

pub fn info() -> Option<&'static PlatformInfo> {
    (PLATFORM.state.load(Ordering::Acquire) == PLATFORM_READY)
        // SAFETY: Acquire observed the Release publication after initialization.
        .then(|| unsafe { (&*PLATFORM.value.get()).assume_init_ref() })
}

/// Write the platform-owned boot params tail from low `.boot.text`.
///
/// # Safety
/// `params` must point to writable low `.boot.bss` storage for
/// `BootPlatformParams`. This function runs before MMU enable and writes only
/// plain integers with inline stores so it does not pull high-linked runtime
/// helpers into `.boot.text`.
#[unsafe(link_section = ".boot.text")]
pub(crate) unsafe fn write_boot_platform_params(
    params: *mut BootPlatformParams,
    info: &BootPlatformInfo,
) {
    let params = params as usize;
    unsafe {
        write_platform_param(params, 0, info.dtb_pa as u64);
        write_platform_param(params, 1, info.dtb_size as u64);
        write_platform_param(params, 2, info.ram.start as u64);
        write_platform_param(params, 3, info.ram.size as u64);
        write_platform_param(params, 4, info.uart.start as u64);
        write_platform_param(params, 5, info.uart.size as u64);
        write_platform_param(params, 6, info.gic_distributor.start as u64);
        write_platform_param(params, 7, info.gic_distributor.size as u64);
        write_platform_param(params, 8, info.gic_cpu_interface.start as u64);
        write_platform_param(params, 9, info.gic_cpu_interface.size as u64);
    }
}

pub(crate) fn dtb_from_boot_params(params: *const BootPlatformParams) -> (usize, u64) {
    unsafe {
        (
            core::ptr::addr_of!((*params).dtb_pa).read_volatile() as usize,
            core::ptr::addr_of!((*params).dtb_size).read_volatile(),
        )
    }
}

pub(crate) fn info_from_boot_params(params: *const BootPlatformParams) -> PlatformInfo {
    unsafe {
        PlatformInfo {
            ram: DeviceRange {
                start: core::ptr::addr_of!((*params).ram_start).read_volatile(),
                size: core::ptr::addr_of!((*params).ram_size).read_volatile(),
            },
            uart: DeviceRange {
                start: core::ptr::addr_of!((*params).uart_start).read_volatile(),
                size: core::ptr::addr_of!((*params).uart_size).read_volatile(),
            },
            gic_distributor: DeviceRange {
                start: core::ptr::addr_of!((*params).gicd_start).read_volatile(),
                size: core::ptr::addr_of!((*params).gicd_size).read_volatile(),
            },
            gic_cpu_interface: DeviceRange {
                start: core::ptr::addr_of!((*params).gicc_start).read_volatile(),
                size: core::ptr::addr_of!((*params).gicc_size).read_volatile(),
            },
        }
    }
}

#[unsafe(link_section = ".boot.text")]
unsafe fn write_platform_param(params: usize, index: usize, value: u64) {
    unsafe {
        boot_store64(
            params.wrapping_add(index.wrapping_mul(core::mem::size_of::<u64>())),
            value,
        )
    };
}

#[unsafe(link_section = ".boot.text")]
unsafe fn boot_store64(addr: usize, value: u64) {
    unsafe {
        core::arch::asm!(
            "str {value}, [{addr}]",
            addr = in(reg) addr,
            value = in(reg) value,
            options(nostack, preserves_flags)
        );
    }
}
