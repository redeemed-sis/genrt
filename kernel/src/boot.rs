use bootinfo::{BootInfo, MemoryRegion, MemoryRegionKind};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::dtb::{MAX_BOOT_MEMORY_REGIONS, parse_memory_regions};

const fn repeated_byte_pattern(byte: usize) -> usize {
    let mut pattern = 0usize;
    let mut shift = 0usize;
    while shift < usize::BITS as usize {
        pattern |= byte << shift;
        shift += 8;
    }
    pattern
}

const BOOT_STACK_CANARY: usize = repeated_byte_pattern(0xA5);

static mut BOOT_INFO: BootInfo = BootInfo::new();
static mut BOOT_MEMORY_MAP: [MemoryRegion; MAX_BOOT_MEMORY_REGIONS] = [MemoryRegion {
    start: 0,
    size: 0,
    kind: MemoryRegionKind::Reserved,
}; MAX_BOOT_MEMORY_REGIONS];
static BOOTINFO_CLAIMED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    static __boot_stack_bottom_value: usize;
    static __boot_stack_top_value: usize;
    fn arch_phys_to_virt(pa: usize) -> usize;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BootstrapStackUsage {
    pub total_bytes: usize,
    pub used_bytes: usize,
    pub unused_bytes: usize,
    pub lowest_used_addr: usize,
}

/// Parse and publish the immutable generic boot information.
///
/// # Arguments
///
/// * `dtb_pa` - Physical address of the firmware-provided DTB.
/// * `dtb_va` - Readable kernel virtual alias of the same DTB.
///
/// # Returns
///
/// Returns the permanently published boot information. Parsing uses fixed
/// boot-owned storage and performs no heap allocation or scheduler blocking.
///
/// # Panics
///
/// Panics if publication was already claimed or DTB memory-map parsing fails.
/// The atomic claim makes the boot-only mutable storage unreachable through a
/// second initialization attempt.
///
/// # Safety
///
/// This must run on the boot CPU before interrupts, scheduler entry, and
/// secondary CPU startup. `dtb_va` must reference the complete DTB containing
/// `dtb_pa` for the duration of parsing.
pub unsafe fn init_bootinfo(dtb_pa: usize, dtb_va: usize) -> &'static BootInfo {
    if BOOTINFO_CLAIMED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        panic!("bootinfo: initialized twice");
    }
    unsafe {
        crate::debug!("bootinfo: parsing dtb");
        let parsed = parse_memory_regions(dtb_va, &mut *core::ptr::addr_of_mut!(BOOT_MEMORY_MAP))
            .unwrap_or_else(|err| panic!("bootinfo: failed to parse DTB memory map: {err:?}"));
        BOOT_INFO.dtb_pa = dtb_pa as u64;
        BOOT_INFO.dtb_size = parsed.dtb_size;
        BOOT_INFO.memory_map = if parsed.region_count == 0 {
            &[]
        } else {
            &(&*core::ptr::addr_of!(BOOT_MEMORY_MAP))[..parsed.region_count]
        };
        &*core::ptr::addr_of!(BOOT_INFO)
    }
}

pub fn bootstrap_stack_usage() -> BootstrapStackUsage {
    let bottom_pa = unsafe { core::ptr::addr_of!(__boot_stack_bottom_value).read_volatile() };
    let top_pa = unsafe { core::ptr::addr_of!(__boot_stack_top_value).read_volatile() };
    let bottom = unsafe { arch_phys_to_virt(bottom_pa) };
    let top = unsafe { arch_phys_to_virt(top_pa) };
    let mut cursor = bottom;

    while cursor < top {
        // SAFETY: the bootstrap stack is a statically linked early-boot region
        // that remains mapped and readable throughout kernel bootstrap.
        let word = unsafe { (cursor as *const usize).read() };
        if word != BOOT_STACK_CANARY {
            break;
        }
        cursor += core::mem::size_of::<usize>();
    }

    BootstrapStackUsage {
        total_bytes: top - bottom,
        used_bytes: top - cursor,
        unused_bytes: cursor - bottom,
        lowest_used_addr: cursor - bottom + bottom_pa,
    }
}
