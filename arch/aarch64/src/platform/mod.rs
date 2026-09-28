//! AArch64 platform ranges discovered by the low boot DTB parser.
//!
//! The first DTB parse happens in `.boot.text`, before MMU enable, because UART
//! and the selected interrupt controller must be mapped before high Rust. This is
//! the high-side storage/validation surface for arch platform ranges; concrete
//! boot-protocol constants live in platform-specific submodules.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, Ordering},
};

mod boot_dtb;
mod cpu;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
pub(crate) mod gic;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
pub(crate) mod virt_interrupts;
#[cfg(feature = "platform-raspberrypi-3b-plus")]
pub(crate) use raspberrypi as interrupt_controller;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
pub(crate) use virt_interrupts as interrupt_controller;
#[cfg(feature = "platform-raspberrypi-3b-plus")]
pub(crate) mod pi_power;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
pub(crate) mod psci;
#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
pub(crate) mod qemu;
#[cfg(feature = "platform-raspberrypi-3b-plus")]
pub(crate) mod raspberrypi;
#[cfg(feature = "platform-raspberrypi-3b-plus")]
mod raspberrypi_cache;
#[cfg(feature = "platform-raspberrypi-3b-plus")]
pub(crate) mod spin_table;
pub(crate) mod uart;

pub(crate) use self::boot_dtb::{BootDeviceRange, BootPlatformInfo};
pub(crate) use self::cpu::{cpu_count, expected_hardware_id, init_cpu_topology};
pub(crate) const CPU_CAPACITY: usize = cpu::MAX_BOOT_CPUS;
/// Low assembly selector for the Raspberry Pi EL2 and spin-table entry path.
pub(crate) const PI_RUNTIME: u8 = cfg!(feature = "platform-raspberrypi-3b-plus") as u8;

/// Parse and complete the platform ranges used by the low MMU builder.
///
/// # Arguments
///
/// * `firmware_dtb` - DTB pointer supplied by firmware, used on Raspberry Pi.
/// * `info` - Zeroed, writable low-boot platform storage to populate.
///
/// # Returns
///
/// Returns nothing. This path is bounded, allocation-free, and stays entirely
/// in `.boot.text`.
///
/// # Safety
///
/// The selected DTB address must point to readable firmware or loader memory.
/// `info` must be writable low-linked storage before translation is enabled.
#[unsafe(link_section = ".boot.text")]
pub(crate) unsafe fn prepare_boot_platform(firmware_dtb: usize, info: &mut BootPlatformInfo) {
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    let dtb_pa = firmware_dtb;
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    let dtb_pa = {
        let _ = firmware_dtb;
        qemu::BOOT_DTB_PA
    };
    unsafe { boot_dtb::parse_boot_platform(dtb_pa, info) };
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    raspberrypi::boot_ranges(info);
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    if !platform_info_complete(info) {
        qemu::apply_fallback_platform_info(info);
    }
}

#[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
#[unsafe(link_section = ".boot.text")]
const fn platform_info_complete(info: &BootPlatformInfo) -> bool {
    info.ram.is_present()
        && info.uart.is_present()
        && info.irq_primary.is_present()
        && info.irq_secondary.is_present()
}

/// Release a secondary CPU using the selected firmware protocol.
///
/// # Arguments
///
/// * `index` - Validated nonzero logical CPU and bootstrap stack slot.
/// * `entry_pa` - Physical address of the shared secondary entry.
/// * `handoff_pa` - Physical address of the published TTBR1 handoff word.
/// * `ttbr1` - Published runtime TTBR1 root for the Pi cache handoff.
///
/// # Returns
///
/// Returns zero on accepted release or a negative architecture error code.
/// This boot-only path is bounded and allocation-free.
pub(crate) fn start_secondary_cpu(
    index: usize,
    entry_pa: usize,
    handoff_pa: usize,
    ttbr1: usize,
) -> i64 {
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    {
        spin_table::start(index, entry_pa, handoff_pa, ttbr1)
    }
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    {
        let _ = (handoff_pa, ttbr1);
        let Some((function_id, target_hardware)) = cpu::psci_cpu_on_call(index) else {
            return -2;
        };
        psci::cpu_on(function_id, target_hardware, entry_pa as u64, index as u64)
    }
}

/// Request a terminal system restart through the selected firmware backend.
///
/// # Returns
///
/// Does not return on success; otherwise returns the architecture error code.
/// Takes no arguments and neither allocates nor blocks.
pub(crate) fn system_restart() -> i64 {
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    {
        pi_power::request(false)
    }
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    {
        psci::system_reset()
    }
}

/// Request a terminal system halt through the selected firmware backend.
///
/// # Returns
///
/// Does not return on success; otherwise returns the architecture error code.
/// Pi halt does not physically remove board power. Takes no arguments and
/// neither allocates nor blocks.
pub(crate) fn system_power_off() -> i64 {
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    {
        pi_power::request(true)
    }
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    {
        psci::system_off()
    }
}

/// Synchronize an unpublished executable user page when the platform needs it.
///
/// # Arguments
///
/// * `pa` - Physical address of the populated 4 KiB user page.
/// * `executable` - Whether the mapping will permit instruction fetch.
///
/// # Returns
///
/// Returns nothing. QEMU has no extra publication step; Pi cleans data to PoU
/// and invalidates instruction caches before executable mapping publication.
/// This operation is allocation-free and runs in task context.
///
/// # Safety
///
/// `pa` must identify the caller-owned, populated 4 KiB physical page. Its
/// direct-map alias must be accessible until publication completes.
#[inline(always)]
pub(crate) unsafe fn prepare_user_page(pa: usize, executable: bool) {
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    if executable {
        unsafe { raspberrypi_cache::synchronize_loaded_code(pa) };
    }
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    let _ = (pa, executable);
}

#[cfg(feature = "platform-raspberrypi-3b-plus")]
static INITRD_START: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(feature = "platform-raspberrypi-3b-plus")]
static INITRD_SIZE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Publish the firmware-loaded initramfs from resident `dtb` before memory init.
/// Returns nothing; panics on missing/invalid Pi handoff. Boot-only, no allocation.
/// QEMU retains its explicit loader slot rather than requiring chosen properties.
pub(crate) fn init_boot_payloads(dtb: &[u8]) {
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    {
        let fdt = fdt_raw::Fdt::from_bytes(dtb).expect("invalid firmware DTB");
        let chosen = fdt.find_by_path("/chosen").expect("missing chosen");
        let address = |name| {
            let prop = chosen.find_property(name).expect("missing initrd handoff");
            let mut cells = prop.as_u32_iter();
            let first = cells.next().expect("empty initrd address") as u64;
            let value = match cells.next() {
                Some(low) => (first << 32) | low as u64,
                None => first,
            };
            assert!(cells.next().is_none(), "invalid initrd address cells");
            usize::try_from(value).expect("initrd address overflow")
        };
        let start = address("linux,initrd-start");
        let end = address("linux,initrd-end");
        assert!(
            start != 0 && end > start && end - start <= 4 * 1024 * 1024,
            "invalid initramfs extent"
        );
        let ram = info().expect("platform unpublished").ram;
        assert!(
            end as u64 <= ram.start + ram.size,
            "initrd outside mapped RAM"
        );
        INITRD_START.store(start, Ordering::Relaxed);
        INITRD_SIZE.store(end - start, Ordering::Release);
    }
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    let _ = dtb;
}

/// Return `(physical_start, size)` of the reserved initramfs handoff.
/// No arguments; boot publication must precede use. Allocation-free and nonblocking.
pub(crate) fn initramfs_range() -> (usize, usize) {
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    {
        let size = INITRD_SIZE.load(Ordering::Acquire);
        (INITRD_START.load(Ordering::Relaxed), size)
    }
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    {
        (qemu::INITRAMFS_LOAD_PA, qemu::INITRAMFS_RESERVED_SIZE)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PlatformError {
    AlreadyInitialized,
    MissingRam,
    MissingUart,
    MissingInterruptController,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DeviceRange {
    pub start: u64,
    pub size: u64,
}

impl DeviceRange {
    pub const fn is_present(self) -> bool {
        self.size != 0
    }
}

/// Platform-specific controller MMIO carried by the generic boot record.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InterruptMap {
    /// QEMU virt distributor and per-CPU interface windows.
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    GicV2 {
        distributor: DeviceRange,
        cpu_interface: DeviceRange,
    },
    /// Pi ARM-local interrupt window; the GPU controller is in `uart` MMIO.
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    Bcm2837 { local: DeviceRange },
}

impl InterruptMap {
    fn is_present(self) -> bool {
        match self {
            #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
            Self::GicV2 {
                distributor,
                cpu_interface,
            } => distributor.is_present() && cpu_interface.is_present(),
            #[cfg(feature = "platform-raspberrypi-3b-plus")]
            Self::Bcm2837 { local } => local.is_present(),
        }
    }

    /// Return the MMIO windows required by the selected controller.
    ///
    /// Takes no arguments beyond `self`. Returns the primary range and an
    /// optional second range for boot and runtime mapping without allocation.
    pub(crate) fn ranges(self) -> (DeviceRange, Option<DeviceRange>) {
        match self {
            #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
            Self::GicV2 {
                distributor,
                cpu_interface,
            } => (distributor, Some(cpu_interface)),
            #[cfg(feature = "platform-raspberrypi-3b-plus")]
            Self::Bcm2837 { local } => (local, None),
        }
    }
}
/// Validated, immutable physical ranges for the selected AArch64 platform.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PlatformInfo {
    pub ram: DeviceRange,
    pub uart: DeviceRange,
    pub interrupts: InterruptMap,
}

impl PlatformInfo {
    fn validate(self) -> Result<Self, PlatformError> {
        if !self.ram.is_present() {
            return Err(PlatformError::MissingRam);
        }
        if !self.uart.is_present() {
            return Err(PlatformError::MissingUart);
        }
        if !self.interrupts.is_present() {
            return Err(PlatformError::MissingInterruptController);
        }
        Ok(self)
    }
}

/// Platform-owned boot parameters discovered by the low DTB parser.
///
/// The two fixed interrupt slots preserve the low assembly layout: QEMU uses
/// distributor then CPU interface; Pi uses only the ARM-local window. Its GPU
/// controller is already covered by the DTB-derived peripheral range.
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
    pub irq_primary_start: u64,
    pub irq_primary_size: u64,
    pub irq_secondary_start: u64,
    pub irq_secondary_size: u64,
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
            irq_primary_start: 0,
            irq_primary_size: 0,
            irq_secondary_start: 0,
            irq_secondary_size: 0,
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
        write_platform_param(params, 6, info.irq_primary.start as u64);
        write_platform_param(params, 7, info.irq_primary.size as u64);
        write_platform_param(params, 8, info.irq_secondary.start as u64);
        write_platform_param(params, 9, info.irq_secondary.size as u64);
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
    let primary = unsafe {
        DeviceRange {
            start: core::ptr::addr_of!((*params).irq_primary_start).read_volatile(),
            size: core::ptr::addr_of!((*params).irq_primary_size).read_volatile(),
        }
    };
    #[cfg(not(feature = "platform-raspberrypi-3b-plus"))]
    let interrupts = unsafe {
        InterruptMap::GicV2 {
            distributor: primary,
            cpu_interface: DeviceRange {
                start: core::ptr::addr_of!((*params).irq_secondary_start).read_volatile(),
                size: core::ptr::addr_of!((*params).irq_secondary_size).read_volatile(),
            },
        }
    };
    #[cfg(feature = "platform-raspberrypi-3b-plus")]
    let interrupts = InterruptMap::Bcm2837 { local: primary };
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
            interrupts,
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
