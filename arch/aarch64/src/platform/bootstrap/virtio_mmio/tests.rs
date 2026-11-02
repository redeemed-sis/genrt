use super::*;
use std::{vec, vec::Vec};

fn discover(bytes: &[u8]) -> Result<Candidates, DiscoveryError> {
    use kernel::dtb::MmioRange;
    let tree = RuntimeDtb::from_bytes(bytes).map_err(resource_error)?;
    let interrupts = GicV2Domain::discover(
        &tree,
        MmioRange {
            start: 0x123000,
            size: 0x1000,
        },
        MmioRange {
            start: 0x124000,
            size: 0x2000,
        },
    )
    .map_err(resource_error)?;
    super::discover(&tree, &interrupts)
}

struct Blob {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

impl Blob {
    fn new() -> Self {
        let mut blob = Self {
            structure: Vec::new(),
            strings: Vec::new(),
        };
        blob.begin("");
        blob.cells("#address-cells", &[2]);
        blob.cells("#size-cells", &[2]);
        blob.cells("interrupt-parent", &[1]);
        blob
    }
    fn token(&mut self, token: u32) {
        self.structure.extend_from_slice(&token.to_be_bytes());
    }
    fn begin(&mut self, name: &str) {
        self.token(1);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        while !self.structure.len().is_multiple_of(4) {
            self.structure.push(0);
        }
    }
    fn end(&mut self) {
        self.token(2);
    }
    fn prop(&mut self, name: &str, data: &[u8]) {
        self.token(3);
        self.token(data.len() as u32);
        self.token(self.strings.len() as u32);
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        self.structure.extend_from_slice(data);
        while !self.structure.len().is_multiple_of(4) {
            self.structure.push(0);
        }
    }
    fn cells(&mut self, name: &str, cells: &[u32]) {
        let data: Vec<_> = cells.iter().flat_map(|cell| cell.to_be_bytes()).collect();
        self.prop(name, &data);
    }
    fn gic(&mut self) {
        self.begin("interrupt-controller@123000");
        self.prop("compatible", b"arm,cortex-a15-gic\0");
        self.cells("reg", &[0, 0x123000, 0, 0x1000, 0, 0x124000, 0, 0x2000]);
        self.cells("phandle", &[1]);
        self.cells("#interrupt-cells", &[3]);
        self.prop("interrupt-controller", &[]);
        self.end();
    }
    fn transport(&mut self, base: u64, size: u64, interrupts: &[u32], status: Option<&str>) {
        self.begin("storage@100000200");
        self.prop("compatible", b"vendor,extra\0virtio,mmio\0");
        self.cells(
            "reg",
            &[
                (base >> 32) as u32,
                base as u32,
                (size >> 32) as u32,
                size as u32,
            ],
        );
        self.cells("interrupts", interrupts);
        if let Some(status) = status {
            let mut value = status.as_bytes().to_vec();
            value.push(0);
            self.prop("status", &value);
        }
        self.end();
    }
    fn finish(mut self) -> Vec<u8> {
        self.end();
        self.token(9);
        let mut result = vec![0; 56];
        let strings_start = 56 + self.structure.len();
        let header = [
            0xd00dfeed,
            (strings_start + self.strings.len()) as u32,
            56,
            strings_start as u32,
            40,
            17,
            16,
            0,
            self.strings.len() as u32,
            self.structure.len() as u32,
        ];
        for (index, cell) in header.into_iter().enumerate() {
            result[index * 4..index * 4 + 4].copy_from_slice(&cell.to_be_bytes());
        }
        result.extend(self.structure);
        result.extend(self.strings);
        result
    }
}

fn one() -> Vec<u8> {
    let mut blob = Blob::new();
    blob.gic();
    blob.transport(0x1_0000_0200, 0x200, &[0, 47, 1], None);
    blob.finish()
}

#[test]
fn discovers_real_64bit_resources_and_spi_offset_not_hardcoded_addresses() {
    let bytes = one();
    let candidates = discover(&bytes).unwrap();
    assert_eq!(
        candidates.as_slice(),
        &[Candidate {
            base: 0x1_0000_0200,
            size: 0x200,
            irq: 79
        }]
    );
}

#[test]
fn disabled_nodes_and_disabled_ancestors_are_not_probed() {
    let mut blob = Blob::new();
    blob.gic();
    blob.transport(0x1000, 0x200, &[0, 1, 4], Some("disabled"));
    blob.begin("disabled-bus");
    blob.prop("status", b"disabled\0");
    blob.transport(0x2000, 0x200, &[0, 2, 4], None);
    blob.end();
    blob.transport(0x3000, 0x200, &[0, 3, 4], Some("okay"));
    let bytes = blob.finish();
    assert_eq!(
        discover(&bytes).unwrap().as_slice(),
        &[Candidate {
            base: 0x3000,
            size: 0x200,
            irq: 35
        }]
    );
}

#[test]
fn rejects_bad_interrupt_shape_type_flags_and_out_of_range_spi() {
    for cells in [
        &[0, 1][..],
        &[1, 1, 4],
        &[0, 988, 4],
        &[0, 1, 0],
        &[0, 1, 8],
        &[0, 1, 4, 0],
    ] {
        let mut blob = Blob::new();
        blob.gic();
        blob.transport(0x1000, 0x200, cells, None);
        assert!(matches!(
            discover(&blob.finish()),
            Err(DiscoveryError::Malformed)
        ));
    }
}

#[test]
fn rejects_bad_reg_size_alignment_overflow_and_nonroot_bus() {
    for (base, size) in [
        (0, 0x200),
        (0x1001, 0x200),
        (0x1000, 0x100),
        (0x1000, 0x1001),
        (u64::MAX - 3, 0x200),
    ] {
        let mut blob = Blob::new();
        blob.gic();
        blob.transport(base, size, &[0, 1, 4], None);
        assert!(matches!(
            discover(&blob.finish()),
            Err(DiscoveryError::Malformed)
        ));
    }
    let mut blob = Blob::new();
    blob.gic();
    blob.begin("bus");
    blob.cells("#address-cells", &[2]);
    blob.cells("#size-cells", &[2]);
    blob.transport(0x1000, 0x200, &[0, 1, 4], None);
    blob.end();
    assert!(matches!(
        discover(&blob.finish()),
        Err(DiscoveryError::Unsupported)
    ));
}

#[test]
fn accepts_shared_pages_but_rejects_overlapping_windows_and_duplicate_irqs() {
    for (base, spi, succeeds) in [(0x1200, 2, true), (0x1100, 2, false), (0x1200, 1, false)] {
        let mut blob = Blob::new();
        blob.gic();
        blob.transport(0x1000, 0x200, &[0, 1, 4], None);
        blob.transport(base, 0x200, &[0, spi, 4], None);
        assert_eq!(discover(&blob.finish()).is_ok(), succeeds);
    }
}

#[test]
fn capacity_is_exactly_32_and_disabled_devices_do_not_consume_slots() {
    for count in [32, 33] {
        let mut blob = Blob::new();
        blob.gic();
        for index in 0..count {
            blob.transport(0x1000 + index * 0x200, 0x200, &[0, index as u32, 4], None);
        }
        blob.transport(0x9000, 0x200, &[0, 40, 4], Some("disabled"));
        let bytes = blob.finish();
        if count == 32 {
            assert_eq!(discover(&bytes).unwrap().as_slice().len(), 32);
        } else {
            assert!(matches!(
                discover(&bytes),
                Err(DiscoveryError::CapacityOrOverlap)
            ));
        }
    }
}

#[test]
fn malformed_headers_structure_and_strings_never_panic_or_succeed_as_empty() {
    let bytes = one();
    for length in 0..bytes.len() {
        assert!(discover(&bytes[..length]).is_err());
    }
    for offset in [0, 4, 8, 12, 20, 24, 32, 36, 56] {
        let mut broken = bytes.clone();
        broken[offset..offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(discover(&broken).is_err(), "offset {offset}");
    }
    let mut broken = bytes.clone();
    // First property name offset must not index outside the string block.
    broken[72..76].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(discover(&broken).is_err());
    let mut broken = bytes;
    // Replace root END_NODE with NOP: no balanced root at FDT_END.
    let structure_size = u32::from_be_bytes(broken[36..40].try_into().unwrap()) as usize;
    broken[56 + structure_size - 8..56 + structure_size - 4].copy_from_slice(&4u32.to_be_bytes());
    assert!(discover(&broken).is_err());
}

#[test]
fn unsupported_root_cells_and_controller_fail_explicitly() {
    let mut bytes = one();
    bytes[76..80].copy_from_slice(&1u32.to_be_bytes());
    assert!(discover(&bytes).is_err());
    let mut blob = Blob::new();
    blob.transport(0x1000, 0x200, &[0, 1, 4], None);
    assert!(matches!(
        discover(&blob.finish()),
        Err(DiscoveryError::Malformed)
    ));
}
