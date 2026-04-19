use bootinfo::{MemoryRegion, MemoryRegionKind};

use super::types::{
    FrameRange, PAGE_SIZE, PhysRange, PhysRegion, RegionKind, align_down, align_up,
};
use super::{MemoryError, Result};

pub(crate) fn collect_boot_ranges(
    regions: &[MemoryRegion],
    ram_out: &mut [PhysRange],
    ram_count: &mut usize,
    reserved_out: &mut [PhysRange],
    reserved_count: &mut usize,
) -> Result<()> {
    for region in regions {
        let Some(range) = phys_range_from_u64(region.start, region.size)? else {
            continue;
        };

        match region.kind {
            MemoryRegionKind::Usable => {
                crate::debug!("memory: dtb usable {:?}", range);
                push_range(ram_out, ram_count, range)?;
            }
            MemoryRegionKind::Reserved => {
                crate::debug!("memory: dtb reserved {:?}", range);
                push_range(reserved_out, reserved_count, range)?;
            }
            MemoryRegionKind::Mmio => {
                crate::trace!("memory: dtb mmio {:?}", range);
            }
        }
    }

    Ok(())
}

pub(crate) fn add_reserved_range(
    reserved_out: &mut [PhysRange],
    reserved_count: &mut usize,
    range: PhysRange,
    label: &str,
) -> Result<()> {
    crate::debug!("memory: reserving {label} {:?}", range);
    push_range(reserved_out, reserved_count, range)
}

pub(crate) fn build_memory_map(
    ram_ranges: &[PhysRange],
    reserved_ranges: &[PhysRange],
    phys_regions: &mut [PhysRegion],
    phys_region_count: &mut usize,
    usable_ranges: &mut [FrameRange],
    usable_range_count: &mut usize,
) -> Result<()> {
    for ram in ram_ranges {
        let mut cursor = ram.start;

        for reserved in reserved_ranges {
            if reserved.end <= ram.start {
                continue;
            }
            if reserved.start >= ram.end {
                break;
            }

            if let Some(clipped) = reserved.clipped_to(*ram) {
                if cursor < clipped.start {
                    let usable = PhysRange {
                        start: cursor,
                        end: clipped.start,
                    };
                    push_phys_region(
                        phys_regions,
                        phys_region_count,
                        PhysRegion {
                            range: usable,
                            kind: RegionKind::Usable,
                        },
                    )?;
                    push_usable_frame_range(usable_ranges, usable_range_count, usable)?;
                }

                push_phys_region(
                    phys_regions,
                    phys_region_count,
                    PhysRegion {
                        range: clipped,
                        kind: RegionKind::Reserved,
                    },
                )?;
                cursor = cursor.max(clipped.end);
            }
        }

        if cursor < ram.end {
            let usable = PhysRange {
                start: cursor,
                end: ram.end,
            };
            push_phys_region(
                phys_regions,
                phys_region_count,
                PhysRegion {
                    range: usable,
                    kind: RegionKind::Usable,
                },
            )?;
            push_usable_frame_range(usable_ranges, usable_range_count, usable)?;
        }
    }

    if *usable_range_count == 0 {
        return Err(MemoryError::NoUsableRam);
    }

    Ok(())
}

pub(crate) fn push_range(out: &mut [PhysRange], count: &mut usize, range: PhysRange) -> Result<()> {
    let slot = out.get_mut(*count).ok_or(MemoryError::TooManyRanges)?;
    *slot = range;
    *count += 1;
    Ok(())
}

pub(crate) fn push_phys_region(
    out: &mut [PhysRegion],
    count: &mut usize,
    region: PhysRegion,
) -> Result<()> {
    if region.range.start >= region.range.end {
        return Ok(());
    }

    if let Some(prev) = count.checked_sub(1).and_then(|idx| out.get_mut(idx))
        && prev.kind == region.kind
        && prev.range.end >= region.range.start
    {
        prev.range.end = prev.range.end.max(region.range.end);
        return Ok(());
    }

    let slot = out.get_mut(*count).ok_or(MemoryError::TooManyRanges)?;
    *slot = region;
    *count += 1;
    Ok(())
}

pub(crate) fn push_usable_frame_range(
    out: &mut [FrameRange],
    count: &mut usize,
    range: PhysRange,
) -> Result<()> {
    let aligned = FrameRange {
        start: align_up(range.start, PAGE_SIZE),
        end: align_down(range.end, PAGE_SIZE),
    };
    if aligned.start >= aligned.end {
        return Ok(());
    }

    if let Some(prev) = count.checked_sub(1).and_then(|idx| out.get_mut(idx))
        && prev.end >= aligned.start
    {
        prev.end = prev.end.max(aligned.end);
        return Ok(());
    }

    let slot = out.get_mut(*count).ok_or(MemoryError::TooManyRanges)?;
    *slot = aligned;
    *count += 1;
    Ok(())
}

#[allow(clippy::manual_swap)]
pub(crate) fn sort_ranges(ranges: &mut [PhysRange], len: usize) {
    for i in 1..len {
        let mut j = i;
        while j > 0 && ranges[j].start < ranges[j - 1].start {
            // `slice::swap()` tripped early-boot UB checks in this no-MMU path;
            // keep the exchange explicit and local here.
            let current = ranges[j];
            ranges[j] = ranges[j - 1];
            ranges[j - 1] = current;
            j -= 1;
        }
    }
}

pub(crate) fn merge_ranges(ranges: &mut [PhysRange], len: usize) -> usize {
    if len == 0 {
        return 0;
    }

    let mut out = 0usize;
    for idx in 1..len {
        let current = ranges[idx];
        let last = &mut ranges[out];
        if last.overlaps(current) || last.end == current.start {
            last.end = last.end.max(current.end);
        } else {
            out += 1;
            ranges[out] = current;
        }
    }

    out + 1
}

pub(crate) fn phys_range_from_u64(start: u64, size: u64) -> Result<Option<PhysRange>> {
    if size == 0 {
        return Ok(None);
    }

    let start = usize::try_from(start).map_err(|_| MemoryError::AddressOutOfRange)?;
    let size = usize::try_from(size).map_err(|_| MemoryError::AddressOutOfRange)?;
    PhysRange::from_start_size(start, size)
        .map(Some)
        .ok_or(MemoryError::AddressOutOfRange)
}
