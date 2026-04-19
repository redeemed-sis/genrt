use bootinfo::{MemoryRegion, MemoryRegionKind};
use fdt_raw::{Fdt, FdtError};

pub(crate) const MAX_BOOT_MEMORY_REGIONS: usize = 32;
const FDT_HEADER_SIZE: usize = 40;

#[derive(Clone)]
pub(crate) enum DtbError {
    BadPointer,
    Truncated,
    Parse(FdtError),
    OutOfRegions,
}

impl core::fmt::Debug for DtbError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadPointer => f.write_str("BadPointer"),
            Self::Truncated => f.write_str("Truncated"),
            Self::Parse(err) => f.debug_tuple("Parse").field(err).finish(),
            Self::OutOfRegions => f.write_str("OutOfRegions"),
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedDtbInfo {
    pub dtb_size: u64,
    pub region_count: usize,
}

pub(crate) unsafe fn parse_memory_regions(
    dtb_pa: usize,
    out: &mut [MemoryRegion; MAX_BOOT_MEMORY_REGIONS],
) -> Result<ParsedDtbInfo, DtbError> {
    clear_regions(out);
    if dtb_pa == 0 {
        return Ok(ParsedDtbInfo {
            dtb_size: 0,
            region_count: 0,
        });
    }

    let dtb = unsafe { dtb_slice_from_pa(dtb_pa as *const u8)? };
    let fdt = Fdt::from_bytes(dtb).map_err(DtbError::Parse)?;
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
    })
}

unsafe fn dtb_slice_from_pa(base: *const u8) -> Result<&'static [u8], DtbError> {
    if base.is_null() {
        return Err(DtbError::BadPointer);
    }

    let total_size = unsafe { read_be_u32_ptr(base.add(4))? } as usize;
    if total_size < FDT_HEADER_SIZE {
        return Err(DtbError::Truncated);
    }

    // SAFETY: early boot passes a DTB blob that stays resident for the life of
    // the kernel. We use the FDT header's total size to bound the slice.
    let raw = core::ptr::slice_from_raw_parts(base, total_size);
    Ok(unsafe { &*raw })
}

fn push_region(
    out: &mut [MemoryRegion; MAX_BOOT_MEMORY_REGIONS],
    count: &mut usize,
    region: MemoryRegion,
) -> Result<(), DtbError> {
    if region.size == 0 {
        return Ok(());
    }

    let slot = out.get_mut(*count).ok_or(DtbError::OutOfRegions)?;
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

unsafe fn read_be_u32_ptr(ptr: *const u8) -> Result<u32, DtbError> {
    let b0 = unsafe { core::ptr::read(ptr) };
    let b1 = unsafe { core::ptr::read(ptr.add(1)) };
    let b2 = unsafe { core::ptr::read(ptr.add(2)) };
    let b3 = unsafe { core::ptr::read(ptr.add(3)) };
    Ok(u32::from_be_bytes([b0, b1, b2, b3]))
}
