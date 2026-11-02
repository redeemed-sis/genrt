//! Shared borrowed runtime DTB view; no hardware policy or device ownership.

#[path = "runtime/validate.rs"]
mod validate;

use fdt_raw::{Fdt, Node};

/// Invalid firmware framing, missing resources or unsupported bus translation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DtbError {
    /// The permanent runtime view has already been published.
    AlreadyInitialized,
    /// Header, structure, property or resource arithmetic is invalid.
    Malformed,
    /// A required property, node or range is absent.
    MissingResource,
    /// The resource cannot be decoded by the supported bus protocol.
    Unsupported,
}

/// Checked physical extent decoded from a device's register resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MmioRange {
    /// CPU physical byte address, after ancestor bus translations.
    pub start: u64,
    /// Nonzero byte length of the complete translated register window.
    pub size: u64,
}

/// Immutable zero-copy FDT view borrowing externally owned resident bytes.
/// Construction validates framing once; domain-specific policy stays with
/// bootstrap consumers. All access is allocation-free and changes no IRQ state.
pub struct RuntimeDtb<'a> {
    fdt: Fdt<'a>,
}

impl<'a> RuntimeDtb<'a> {
    /// Construct a shared runtime view without allocating or touching hardware.
    /// # Arguments
    /// * `bytes` - Complete immutable blob; it must outlive the returned view.
    /// # Returns
    /// A borrowed reader with checked structure, strings and reservation map.
    /// # Errors
    /// Malformed for invalid framing, Unsupported for paths beyond the parser's
    /// fixed representation. No device-specific policy is checked.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, DtbError> {
        let fdt = Fdt::from_bytes(bytes).map_err(|_| DtbError::Malformed)?;
        validate::validate(&fdt, bytes)?;
        Ok(Self { fdt })
    }

    /// Borrow the parsed FDT for domain-specific property interpretation.
    /// # Returns
    /// The same immutable reader; no revalidation, allocation or hardware work.
    pub fn fdt(&self) -> &Fdt<'a> {
        &self.fdt
    }

    /// Traverse enabled nodes, suppressing descendants of disabled ancestors.
    /// # Returns
    /// Firmware-order nodes. Call `fdt().all_nodes()` explicitly when adopting
    /// firmware-configured hardware whose DT nodes may be disabled.
    pub fn enabled_nodes(&self) -> impl Iterator<Item = Node<'a>> {
        let mut enabled = [false; 16];
        self.fdt.all_nodes().filter(move |node| {
            let depth = node.level();
            let local = node
                .find_property("status")
                .is_none_or(|p| matches!(p.as_str(), Some("ok" | "okay")));
            enabled[depth] = local && (depth == 0 || enabled[depth - 1]);
            enabled[depth]
        })
    }

    /// Decode and translate one complete MMIO extent through ancestor ranges.
    /// Missing bus `ranges` is not treated as an identity mapping. CPU `reg`
    /// values without sizes should be read through the raw FDT instead.
    /// # Arguments
    /// * `node` - Device node from this reader.
    /// * `index` - Zero-based register-window index.
    /// # Returns
    /// A checked physical extent. Parsing is allocation-free, without I/O.
    /// # Errors
    /// MissingResource for absent `reg`/range; Malformed for invalid lengths or
    /// overflow; Unsupported for cell widths beyond two or absent translation.
    pub fn mmio_reg(&self, node: &Node<'a>, index: usize) -> Result<MmioRange, DtbError> {
        let property = node.find_property("reg").ok_or(DtbError::MissingResource)?;
        let chain = self.ancestor_buses(property.data().as_slice())?;
        let parent = chain.entries[chain.depth - 1];
        if !(1..=2).contains(&parent.address_cells) || !(1..=2).contains(&parent.size_cells) {
            return Err(DtbError::Unsupported);
        }
        let stride = (usize::from(parent.address_cells) + usize::from(parent.size_cells)) * 4;
        if !property.len().is_multiple_of(stride) {
            return Err(DtbError::Malformed);
        }
        let reg = node
            .reg()
            .and_then(|mut r| r.nth(index))
            .ok_or(DtbError::MissingResource)?;
        let size = reg.size.filter(|n| *n != 0).ok_or(DtbError::Malformed)?;
        let mut start = reg.address;
        start.checked_add(size).ok_or(DtbError::Malformed)?;
        for bus in chain.entries[1..chain.depth].iter().rev() {
            start = bus.translate(start, size)?;
        }
        Ok(MmioRange { start, size })
    }

    // Retain only borrowed bus metadata. Nested find_by_path calls keep large
    // dependency parser frames live and can exhaust a debug kernel stack.
    fn ancestor_buses(&self, target_reg: &[u8]) -> Result<BusChain<'a>, DtbError> {
        let mut chain = BusChain {
            entries: [Bus {
                ranges: None,
                address_cells: 0,
                parent_cells: 0,
                size_cells: 0,
            }; 16],
            depth: 0,
        };
        for node in self.fdt.all_nodes() {
            let depth = node.level();
            if node
                .find_property("reg")
                .is_some_and(|p| p.data().as_slice().as_ptr() == target_reg.as_ptr())
            {
                if depth == 0 {
                    return Err(DtbError::Unsupported);
                }
                chain.depth = depth;
                return Ok(chain);
            }
            chain.entries[depth] = Bus {
                ranges: node.find_property("ranges").map(|p| p.data().as_slice()),
                address_cells: node.address_cells,
                parent_cells: if depth == 0 {
                    2
                } else {
                    chain.entries[depth - 1].address_cells
                },
                size_cells: node.size_cells,
            };
        }
        Err(DtbError::MissingResource)
    }
}

struct BusChain<'a> {
    entries: [Bus<'a>; 16],
    depth: usize,
}

#[derive(Clone, Copy)]
struct Bus<'a> {
    ranges: Option<&'a [u8]>,
    address_cells: u8,
    parent_cells: u8,
    size_cells: u8,
}

impl Bus<'_> {
    fn translate(self, start: u64, size: u64) -> Result<u64, DtbError> {
        let ranges = self.ranges.ok_or(DtbError::Unsupported)?;
        if ranges.is_empty() {
            return Ok(start);
        }
        let widths = [self.address_cells, self.parent_cells, self.size_cells];
        if widths.iter().any(|n| !(1..=2).contains(n)) {
            return Err(DtbError::Unsupported);
        }
        let stride = widths.iter().map(|n| usize::from(*n) * 4).sum::<usize>();
        if !ranges.len().is_multiple_of(stride) {
            return Err(DtbError::Malformed);
        }
        let mut cells = ranges
            .chunks_exact(4)
            .map(|word| u32::from_be_bytes(word.try_into().unwrap()));
        while let Some(first) = cells.next() {
            let child = read_cells(core::iter::once(first).chain(cells.by_ref()), widths[0])?;
            let parent = read_cells(cells.by_ref(), widths[1])?;
            let length = read_cells(cells.by_ref(), widths[2])?;
            child.checked_add(length).ok_or(DtbError::Malformed)?;
            parent.checked_add(length).ok_or(DtbError::Malformed)?;
            if let Some(offset) = start.checked_sub(child)
                && offset.checked_add(size).is_some_and(|end| end <= length)
            {
                return parent.checked_add(offset).ok_or(DtbError::Malformed);
            }
        }
        Err(DtbError::MissingResource)
    }
}

fn read_cells(mut cells: impl Iterator<Item = u32>, count: u8) -> Result<u64, DtbError> {
    let mut value = 0;
    for _ in 0..count {
        value = (value << 32) | u64::from(cells.next().ok_or(DtbError::Malformed)?);
    }
    Ok(value)
}
