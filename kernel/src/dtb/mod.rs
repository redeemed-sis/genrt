//! Shared runtime firmware reader and generic boot memory interpretation.

mod runtime;
pub use runtime::{DtbError, MmioRange, RuntimeDtb};

use bootinfo::{MemoryRegion, MemoryRegionKind};
use fdt_raw::Fdt;

static RUNTIME: crate::sync::OncePublication<RuntimeDtb<'static>> =
    crate::sync::OncePublication::new();

/// Validate and publish the one immutable runtime firmware view.
/// Bootstrap-only; performs no heap allocation, hardware I/O or IRQ changes.
/// # Arguments
/// * `bytes` - Permanently resident immutable DTB, mapped across VM transitions.
/// # Returns
/// The shared reader used by early boot and late device discovery.
/// # Errors
/// Invalid DTB framing or AlreadyInitialized on repeated publication.
pub fn init(bytes: &'static [u8]) -> Result<&'static RuntimeDtb<'static>, DtbError> {
    let tree = RuntimeDtb::from_bytes(bytes)?;
    RUNTIME
        .publish(tree)
        .map_err(|_| DtbError::AlreadyInitialized)?;
    Ok(RUNTIME.get().expect("DTB just published"))
}

/// Borrow the permanent runtime view without reparsing or allocation.
/// # Returns
/// The immutable published reader, or None before bootstrap publication.
pub fn runtime() -> Option<&'static RuntimeDtb<'static>> {
    RUNTIME.get()
}

pub(crate) const MAX_BOOT_MEMORY_REGIONS: usize = 32;

/// Generic boot memory-map output exceeded its fixed boot-owned capacity.
#[derive(Clone, Debug)]
pub(crate) enum MemoryMapError {
    /// More nonempty usable/reserved regions than the boot map can retain.
    OutOfRegions,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
/// Summary of the bounded generic DTB parse.
pub(crate) struct ParsedDtbInfo {
    /// Total resident DTB byte length from its header.
    pub dtb_size: u64,
    /// Number of initialized entries in the caller's memory-region array.
    pub region_count: usize,
    /// Number of enabled direct CPU children described below `/cpus`.
    pub cpu_count: usize,
}

/// Parse boot memory regions and enabled CPU count from a resident DTB.
///
/// The parser clears and fills caller-owned fixed storage. It performs no heap
/// allocation, scheduler operation, or IRQ-state change.
///
/// # Arguments
///
/// * `dtb` - Shared, already validated runtime firmware reader.
/// * `out` - Fixed output array receiving sorted usable and reserved regions.
///
/// # Returns
///
/// Returns DTB size, initialized region count, and enabled CPU count.
///
/// # Errors
///
/// Returns [`MemoryMapError`] for region output-capacity exhaustion.
pub(crate) fn parse_memory_regions(
    dtb: &RuntimeDtb<'_>,
    out: &mut [MemoryRegion; MAX_BOOT_MEMORY_REGIONS],
) -> Result<ParsedDtbInfo, MemoryMapError> {
    clear_regions(out);
    let fdt = dtb.fdt();
    let cpu_count = enabled_cpu_count(fdt);
    let mut count = 0usize;

    for reservation in fdt.memory_reservations() {
        push_region(
            out,
            &mut count,
            MemoryRegion {
                start: reservation.address,
                size: reservation.size,
                kind: MemoryRegionKind::Reserved,
            },
        )?;
    }

    for memory in fdt.memory() {
        for region in memory.regions() {
            push_region(
                out,
                &mut count,
                MemoryRegion {
                    start: region.address,
                    size: region.size,
                    kind: MemoryRegionKind::Usable,
                },
            )?;
        }
    }

    for node in fdt.reserved_memory() {
        let Some(regions) = node.reg() else {
            continue;
        };

        for info in regions {
            let Some(size) = info.size else {
                continue;
            };

            push_region(
                out,
                &mut count,
                MemoryRegion {
                    start: info.address,
                    size,
                    kind: MemoryRegionKind::Reserved,
                },
            )?;
        }
    }

    sort_regions_by_start(out, count);

    Ok(ParsedDtbInfo {
        dtb_size: fdt.header().totalsize as u64,
        region_count: count,
        cpu_count,
    })
}

fn enabled_cpu_count(fdt: &Fdt<'_>) -> usize {
    fdt.find_children_by_path("/cpus")
        .filter(|node| node.find_property_str("device_type") == Some("cpu"))
        .filter(|node| {
            node.find_property_str("status")
                .is_none_or(|status| matches!(status, "ok" | "okay"))
        })
        .count()
}

fn push_region(
    out: &mut [MemoryRegion; MAX_BOOT_MEMORY_REGIONS],
    count: &mut usize,
    region: MemoryRegion,
) -> Result<(), MemoryMapError> {
    if region.size == 0 {
        return Ok(());
    }

    let slot = out.get_mut(*count).ok_or(MemoryMapError::OutOfRegions)?;
    *slot = region;
    *count += 1;
    Ok(())
}

fn sort_regions_by_start(out: &mut [MemoryRegion; MAX_BOOT_MEMORY_REGIONS], len: usize) {
    for i in 1..len {
        let mut j = i;
        while j > 0 && region_before(out[j], out[j - 1]) {
            out.swap(j, j - 1);
            j -= 1;
        }
    }
}

fn region_before(lhs: MemoryRegion, rhs: MemoryRegion) -> bool {
    lhs.start < rhs.start || (lhs.start == rhs.start && (lhs.kind as u8) < (rhs.kind as u8))
}

fn clear_regions(out: &mut [MemoryRegion; MAX_BOOT_MEMORY_REGIONS]) {
    for slot in out {
        *slot = MemoryRegion {
            start: 0,
            size: 0,
            kind: MemoryRegionKind::Reserved,
        };
    }
}
