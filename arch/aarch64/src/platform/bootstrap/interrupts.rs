//! Resolve device interrupts against the controller already selected at boot.

use fdt_raw::Node;
use kernel::dtb::{DtbError, MmioRange, RuntimeDtb};

/// Firmware identity of the platform's existing GICv2 interrupt domain.
pub(super) struct GicV2Domain {
    phandle: u32,
}

impl GicV2Domain {
    /// Resolve the selected controller once before probing late devices.
    /// # Arguments
    /// * `tree` - Shared runtime DTB reader.
    /// * `distributor` - Already mapped, selected distributor extent.
    /// * `cpu_interface` - Already mapped, selected CPU-interface extent.
    /// # Returns
    /// The controller identity; no allocation, initialization or register I/O.
    /// # Errors
    /// MissingResource if absent, Unsupported for invalid controller shape,
    /// or Malformed for invalid register/property resources.
    pub(super) fn discover(
        tree: &RuntimeDtb<'_>,
        distributor: MmioRange,
        cpu_interface: MmioRange,
    ) -> Result<Self, DtbError> {
        for node in tree.enabled_nodes().filter(|node| {
            node.compatibles()
                .any(|name| matches!(name, "arm,cortex-a15-gic" | "arm,gic-400"))
        }) {
            if tree.mmio_reg(&node, 0)? != distributor || tree.mmio_reg(&node, 1)? != cpu_interface
            {
                continue;
            }
            if node
                .find_property("#interrupt-cells")
                .and_then(|p| p.as_u32())
                != Some(3)
                || node.find_property("interrupt-controller").is_none()
            {
                return Err(DtbError::Unsupported);
            }
            let phandle = node
                .find_property("phandle")
                .and_then(|p| p.as_u32())
                .filter(|value| *value != 0)
                .ok_or(DtbError::MissingResource)?;
            return Ok(Self { phandle });
        }
        Err(DtbError::MissingResource)
    }

    /// Decode a polling device's SPI in this already-resolved domain.
    /// # Arguments
    /// * `node` - Device node whose inherited interrupt parent must match.
    /// # Returns
    /// Architectural interrupt ID, without enabling or accessing the controller.
    /// # Errors
    /// Unsupported for another controller; Malformed for invalid SPI cells.
    pub(super) fn spi(&self, node: &Node<'_>) -> Result<u32, DtbError> {
        if node.interrupt_parent() != Some(self.phandle.into()) {
            return Err(DtbError::Unsupported);
        }
        let property = node
            .find_property("interrupts")
            .ok_or(DtbError::MissingResource)?;
        let mut cells = property.as_u32_iter();
        let (kind, number, flags) = (cells.next(), cells.next(), cells.next());
        match (kind, number, flags, cells.next()) {
            (Some(0), Some(number @ 0..988), Some(1 | 4), None) if property.len() == 12 => {
                Ok(number + 32)
            }
            _ => Err(DtbError::Malformed),
        }
    }
}
