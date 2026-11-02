//! Bootstrap matching for virtio-mmio devices using shared firmware resources.

use super::interrupts::GicV2Domain;
use kernel::dtb::{DtbError, RuntimeDtb};

const MAX_CANDIDATES: usize = 32;

/// Malformed or unsupported firmware resource description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DiscoveryError {
    /// Header, structure, string, cell, range or interrupt data is invalid.
    Malformed,
    /// Bus translation or the selected GICv2 domain does not match.
    Unsupported,
    /// Enabled virtio resources exceed bounded capacity or overlap.
    CapacityOrOverlap,
}

/// One validated, enabled virtio-mmio window and masked SPI identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Candidate {
    /// CPU physical byte address after bus translation.
    pub(super) base: usize,
    /// Register window size in bytes, at most one page per QEMU transport.
    pub(super) size: usize,
    /// Architectural GIC interrupt ID, with the SPI base of 32 included.
    pub(super) irq: u32,
}

/// Fixed-capacity candidate set; iteration borrows initialized entries only.
pub(super) struct Candidates {
    entries: [Candidate; MAX_CANDIDATES],
    count: usize,
}

impl Candidates {
    /// Borrow the validated discovery result without allocation or I/O.
    /// # Returns
    /// The enabled candidates in firmware order, including an empty slice.
    pub(super) fn as_slice(&self) -> &[Candidate] {
        &self.entries[..self.count]
    }
}

/// Collect enabled virtio windows using shared DTB resource decoding.
/// # Arguments
/// * `tree` - Shared, already validated runtime reader.
/// * `interrupts` - Previously resolved platform interrupt domain.
/// # Returns
/// At most 32 nonoverlapping transports, in firmware order. No I/O/allocation.
/// # Errors
/// Invalid MMIO/IRQ resources, unsupported translations or bounded capacity.
pub(super) fn discover(
    tree: &RuntimeDtb<'_>,
    interrupts: &GicV2Domain,
) -> Result<Candidates, DiscoveryError> {
    let mut result = Candidates {
        entries: [Candidate {
            base: 0,
            size: 0,
            irq: 0,
        }; MAX_CANDIDATES],
        count: 0,
    };
    for node in tree
        .enabled_nodes()
        .filter(|node| node.compatibles().any(|name| name == "virtio,mmio"))
    {
        let range = tree.mmio_reg(&node, 0).map_err(resource_error)?;
        let base = usize::try_from(range.start).map_err(|_| DiscoveryError::Malformed)?;
        let size = usize::try_from(range.size).map_err(|_| DiscoveryError::Malformed)?;
        if base == 0
            || !base.is_multiple_of(4)
            || !(0x108..=4096).contains(&size)
            || base.checked_add(size).is_none()
        {
            return Err(DiscoveryError::Malformed);
        }
        let irq = interrupts.spi(&node).map_err(resource_error)?;
        if result.count == MAX_CANDIDATES
            || result.as_slice().iter().any(|prior| {
                base < prior.base + prior.size && prior.base < base + size || irq == prior.irq
            })
        {
            return Err(DiscoveryError::CapacityOrOverlap);
        }
        result.entries[result.count] = Candidate { base, size, irq };
        result.count += 1;
    }
    Ok(result)
}

fn resource_error(error: DtbError) -> DiscoveryError {
    match error {
        DtbError::Unsupported => DiscoveryError::Unsupported,
        _ => DiscoveryError::Malformed,
    }
}

#[cfg(test)]
#[path = "virtio_mmio/tests.rs"]
mod tests;
