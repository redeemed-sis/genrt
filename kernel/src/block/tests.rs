use super::*;
use alloc::{sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

struct MemoryDevice {
    bytes: Vec<u8>,
    fail: Option<BlockError>,
    calls: Arc<AtomicUsize>,
}

impl MemoryDevice {
    fn new(sectors: usize) -> Self {
        Self {
            bytes: vec![0; sectors * SECTOR_SIZE],
            fail: None,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn io(&self, sector: u64, len: usize) -> Result<usize, BlockError> {
        validate_transfer(self.capacity_sectors(), sector, len)?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        if let Some(error) = self.fail {
            return Err(error);
        }
        Ok(sector as usize * SECTOR_SIZE)
    }
}

impl BlockDevice for MemoryDevice {
    fn capacity_sectors(&self) -> u64 {
        (self.bytes.len() / SECTOR_SIZE) as u64
    }
    fn is_read_only(&self) -> bool {
        self.fail == Some(BlockError::ReadOnly)
    }
    fn read_sectors(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        let start = self.io(sector, buffer.len())?;
        buffer.copy_from_slice(&self.bytes[start..start + buffer.len()]);
        Ok(())
    }
    fn write_sectors(&mut self, sector: u64, buffer: &[u8]) -> Result<(), BlockError> {
        let start = self.io(sector, buffer.len())?;
        self.bytes[start..start + buffer.len()].copy_from_slice(buffer);
        Ok(())
    }
    fn sync(&mut self) -> Result<(), BlockError> {
        self.fail.map_or(Ok(()), Err)
    }
}

fn entry(start: u64, sectors: u64) -> PartitionEntry {
    PartitionEntry {
        bootable: false,
        partition_type: 0x0c,
        start_lba: start,
        sectors,
    }
}

fn mbr() -> [u8; SECTOR_SIZE] {
    let mut bytes = [0; SECTOR_SIZE];
    bytes[510..].copy_from_slice(&[0x55, 0xaa]);
    bytes
}

fn put(bytes: &mut [u8; SECTOR_SIZE], index: usize, kind: u8, start: u32, count: u32) {
    let offset = 446 + index * 16;
    bytes[offset + 4] = kind;
    bytes[offset + 8..offset + 12].copy_from_slice(&start.to_le_bytes());
    bytes[offset + 12..offset + 16].copy_from_slice(&count.to_le_bytes());
}

#[test]
fn transfer_validation_covers_empty_alignment_limit_and_overflow() {
    assert_eq!(validate_transfer(10, 10, 0), Ok(0));
    assert_eq!(validate_transfer(10, 11, 0), Err(BlockError::OutOfBounds));
    assert_eq!(
        validate_transfer(10, 0, 511),
        Err(BlockError::InvalidBuffer)
    );
    assert_eq!(
        validate_transfer(1000, 0, 129 * 512),
        Err(BlockError::TransferTooLarge)
    );
    assert_eq!(validate_transfer(128, 0, 128 * 512), Ok(128));
    assert_eq!(
        validate_transfer(u64::MAX, u64::MAX, 512),
        Err(BlockError::OutOfBounds)
    );
    assert_eq!(validate_transfer(10, 9, 1024), Err(BlockError::OutOfBounds));
}

#[test]
fn partition_translation_bounds_and_noop_do_not_touch_parent() {
    let mut device = MemoryDevice::new(10);
    device.bytes[..512].fill(1);
    let calls = device.calls.clone();
    let mut partition = PartitionDevice::new(Box::new(device), entry(2, 3)).unwrap();
    assert_eq!(partition.capacity_sectors(), 3);
    partition.write_sectors(2, &[7; 512]).unwrap();
    let mut bytes = [0; 512];
    partition.read_sectors(2, &mut bytes).unwrap();
    assert_eq!(bytes, [7; 512]);
    let before = calls.load(Ordering::Relaxed);
    assert_eq!(
        partition.read_sectors(3, &mut bytes),
        Err(BlockError::OutOfBounds)
    );
    assert_eq!(
        partition.write_sectors(u64::MAX, &[0; 512]),
        Err(BlockError::OutOfBounds)
    );
    assert_eq!(
        partition.write_sectors(0, &[0; 1]),
        Err(BlockError::InvalidBuffer)
    );
    partition.read_sectors(3, &mut []).unwrap();
    partition.write_sectors(3, &[]).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), before);
}

#[test]
fn partition_rejects_invalid_manually_constructed_extents() {
    for extent in [entry(0, 2), entry(1, 0), entry(9, 2), entry(u64::MAX, 2)] {
        assert!(matches!(
            PartitionDevice::new(Box::new(MemoryDevice::new(10)), extent),
            Err(BlockError::OutOfBounds)
        ));
    }
}

#[test]
fn injected_io_errors_propagate_without_modifying_destination() {
    for error in [
        BlockError::Io,
        BlockError::Timeout,
        BlockError::Poisoned,
        BlockError::ReadOnly,
    ] {
        let mut device = MemoryDevice::new(10);
        device.fail = Some(error);
        assert_eq!(Mbr::read(&mut device), Err(error));
        let mut partition = PartitionDevice::new(Box::new(device), entry(1, 9)).unwrap();
        assert_eq!(partition.is_read_only(), error == BlockError::ReadOnly);
        let mut buffer = [42; 512];
        assert_eq!(partition.read_sectors(0, &mut buffer), Err(error));
        assert_eq!(buffer, [42; 512]);
        assert_eq!(partition.write_sectors(0, &buffer), Err(error));
        assert_eq!(partition.sync(), Err(error));
    }
}

#[test]
fn mbr_selects_first_fat32_in_table_order_and_accepts_adjacent_extents() {
    let mut bytes = mbr();
    put(&mut bytes, 0, 0x83, 1, 5);
    put(&mut bytes, 1, 0x0b, 6, 4);
    put(&mut bytes, 2, 0x0c, 10, 10);
    bytes[446] = 0x80;
    let table = Mbr::parse(&bytes, 20).unwrap();
    assert!(table.entries()[0].unwrap().bootable);
    assert_eq!(
        table.first_fat32(),
        Some(PartitionEntry {
            partition_type: 0x0b,
            ..entry(6, 4)
        })
    );
    let mut device = MemoryDevice::new(20);
    device.bytes[..512].copy_from_slice(&bytes);
    assert_eq!(Mbr::read(&mut device).unwrap(), table);
    assert!(Mbr::parse(&mbr(), 20).unwrap().first_fat32().is_none());
}

#[test]
fn mbr_validates_signature_all_slots_flags_empty_and_ranges() {
    let mut bytes = mbr();
    bytes[510] = 0;
    assert_eq!(Mbr::parse(&bytes, 20), Err(BlockError::InvalidMbr));
    for (kind, start, count) in [(0x0b, 0, 1), (0x0b, 1, 0), (0x0c, 19, 2), (0, 1, 1)] {
        let mut bytes = mbr();
        put(&mut bytes, 0, 0x0b, 1, 2);
        put(&mut bytes, 3, kind, start, count);
        assert_eq!(Mbr::parse(&bytes, 20), Err(BlockError::InvalidMbr));
    }
    for flag in [1, 0x7f, 0x81, 0xff] {
        let mut bytes = mbr();
        bytes[446] = flag;
        assert_eq!(Mbr::parse(&bytes, 20), Err(BlockError::InvalidMbr));
    }
    let mut bytes = mbr();
    bytes[446] = 0x80;
    assert_eq!(Mbr::parse(&bytes, 20), Err(BlockError::InvalidMbr));
}

#[test]
fn mbr_rejects_overlap_containment_and_handles_32bit_lba_sum() {
    for (start, count) in [(1, 1), (5, 10), (10, 1), (2, 20)] {
        let mut bytes = mbr();
        put(&mut bytes, 0, 0x0b, 1, 10);
        put(&mut bytes, 3, 0x83, start, count);
        assert_eq!(Mbr::parse(&bytes, 100), Err(BlockError::InvalidMbr));
    }
    let mut bytes = mbr();
    put(&mut bytes, 3, 0x0c, u32::MAX, u32::MAX);
    assert!(Mbr::parse(&bytes, 2 * u32::MAX as u64).is_ok());
    assert_eq!(
        Mbr::parse(&bytes, u32::MAX as u64),
        Err(BlockError::InvalidMbr)
    );
}

#[test]
fn boot_registry_transfers_owner_once_and_returns_rejected_owner() {
    assert!(take_boot_devices().iter().all(Option::is_none));
    for index in 0..MAX_BOOT_DEVICES {
        assert!(install_boot_device(Box::new(MemoryDevice::new(index + 2))).is_ok());
    }
    let rejected = install_boot_device(Box::new(MemoryDevice::new(3)))
        .err()
        .unwrap();
    assert_eq!(rejected.capacity_sectors(), 3);
    for (index, device) in take_boot_devices().into_iter().enumerate() {
        assert_eq!(device.unwrap().capacity_sectors(), (index + 2) as u64);
    }
    assert!(take_boot_devices().iter().all(Option::is_none));
}
