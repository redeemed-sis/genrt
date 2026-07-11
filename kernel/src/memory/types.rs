pub type PhysAddr = usize;
pub type VirtAddr = usize;

pub const PAGE_SIZE: usize = 4096;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RegionKind {
    Usable,
    Reserved,
    Mmio,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AddrRange<A> {
    pub start: A,
    pub end: A,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AddrRegion<A> {
    pub range: AddrRange<A>,
    pub kind: RegionKind,
}

pub type PhysRange = AddrRange<PhysAddr>;
pub type PhysRegion = AddrRegion<PhysAddr>;
pub type VirtRange = AddrRange<VirtAddr>;
pub type VirtRegion = AddrRegion<VirtAddr>;
pub type FrameRange = AddrRange<PhysAddr>;

impl AddrRange<usize> {
    pub(crate) const fn empty() -> Self {
        Self { start: 0, end: 0 }
    }

    pub(crate) fn from_start_size(start: usize, size: usize) -> Option<Self> {
        let end = start.checked_add(size)?;
        if start >= end {
            return None;
        }

        Some(Self { start, end })
    }

    pub(crate) fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub(crate) fn clipped_to(self, outer: Self) -> Option<Self> {
        let start = self.start.max(outer.start);
        let end = self.end.min(outer.end);
        if start >= end {
            return None;
        }

        Some(Self { start, end })
    }

    pub(crate) fn frame_count(self) -> usize {
        (self.end - self.start) / PAGE_SIZE
    }
}

/// Round `value` up to the next `align` boundary.
///
/// # Arguments
///
/// * `value` - Address or size to round up.
/// * `align` - Required alignment. `0` is treated as "no alignment" and returns
///   `value`.
///
/// # Returns
///
/// Returns `Some(aligned)` when the rounded value fits in `usize`. Returns
/// `None` if rounding would overflow.
#[inline(always)]
pub(crate) fn align_up(value: usize, align: usize) -> Option<usize> {
    if align == 0 {
        return Some(value);
    }
    Some(value.checked_add(align.checked_sub(1)?)? / align * align)
}

/// Round `value` down to the previous `align` boundary.
///
/// # Arguments
///
/// * `value` - Address or size to round down.
/// * `align` - Required alignment. `0` is treated as "no alignment" and returns
///   `value`.
///
/// # Returns
///
/// Returns the greatest aligned value that is less than or equal to `value`.
#[inline(always)]
pub(crate) fn align_down(value: usize, align: usize) -> usize {
    if align == 0 {
        return value;
    }
    (value / align) * align
}
