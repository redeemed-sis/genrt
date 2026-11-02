use alloc::boxed::Box;

use super::{BlockDevice, BlockError, SECTOR_SIZE, validate_transfer};

/// One validated primary MBR partition; all addresses are in 512-byte sectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PartitionEntry {
    /// Whether this entry has the MBR active flag (0x80).
    pub bootable: bool,
    /// MBR partition type, with 0x0b and 0x0c denoting FAT32.
    pub partition_type: u8,
    /// First sector relative to the parent device; never sector zero.
    pub start_lba: u64,
    /// Nonzero partition length in sectors.
    pub sectors: u64,
}

/// Validated four-entry primary MBR table; extended/GPT tables are not followed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mbr {
    entries: [Option<PartitionEntry>; 4],
}

impl Mbr {
    /// Read and validate sector zero using stack storage, without allocation.
    /// Ordinary-thread only; inherits the driver's bounded I/O deadline.
    /// # Arguments
    /// * `device` - Exclusive device to inspect; no ownership transfer occurs.
    /// # Returns
    /// A table whose nonempty primary partitions are in bounds and disjoint.
    /// # Errors
    /// Returns driver errors unchanged, or [`BlockError::InvalidMbr`] for an
    /// invalid table. No partition or filesystem content is modified.
    pub fn read(device: &mut dyn BlockDevice) -> Result<Self, BlockError> {
        let mut sector = [0u8; SECTOR_SIZE];
        device.read_sectors(0, &mut sector)?;
        Self::parse(&sector, device.capacity_sectors())
    }

    /// Validate all four entries, even those following a usable FAT32 entry.
    /// This bounded parser neither allocates nor performs I/O or IRQ changes.
    /// # Arguments
    /// * `sector` - Complete 512-byte MBR sector.
    /// * `capacity` - Parent device capacity in sectors.
    /// # Returns
    /// A validated table, possibly with no partitions or no FAT32 partition.
    /// # Errors
    /// Returns [`BlockError::InvalidMbr`] for a bad signature, illegal boot
    /// flag, inconsistent empty entry, zero/out-of-range extent, or overlap.
    pub fn parse(sector: &[u8; SECTOR_SIZE], capacity: u64) -> Result<Self, BlockError> {
        if sector[510..] != [0x55, 0xaa] {
            return Err(BlockError::InvalidMbr);
        }
        let mut entries: [Option<PartitionEntry>; 4] = [None; 4];
        for (index, raw) in sector[446..510].chunks_exact(16).enumerate() {
            let bootable = match raw[0] {
                0 => false,
                0x80 => true,
                _ => return Err(BlockError::InvalidMbr),
            };
            let start = u32::from_le_bytes(raw[8..12].try_into().unwrap()) as u64;
            let sectors = u32::from_le_bytes(raw[12..16].try_into().unwrap()) as u64;
            if raw[4] == 0 {
                if bootable || start != 0 || sectors != 0 {
                    return Err(BlockError::InvalidMbr);
                }
                continue;
            }
            let end = start.checked_add(sectors).ok_or(BlockError::InvalidMbr)?;
            if start == 0 || sectors == 0 || end > capacity {
                return Err(BlockError::InvalidMbr);
            }
            for prior in entries[..index].iter().flatten() {
                if start < prior.start_lba + prior.sectors && prior.start_lba < end {
                    return Err(BlockError::InvalidMbr);
                }
            }
            entries[index] = Some(PartitionEntry {
                bootable,
                partition_type: raw[4],
                start_lba: start,
                sectors,
            });
        }
        Ok(Self { entries })
    }

    /// Inspect the original four slots without allocation, I/O, or IRQ changes.
    /// # Returns
    /// A borrowed array with `None` for empty primary entries.
    pub fn entries(&self) -> &[Option<PartitionEntry>; 4] {
        &self.entries
    }

    /// Select the first type 0x0b/0x0c entry in table order, ignoring active flag.
    /// Allocation-free; does not inspect or mount filesystem content.
    /// # Returns
    /// A copied FAT32 entry, or `None` if no primary FAT32 partition exists.
    pub fn first_fat32(&self) -> Option<PartitionEntry> {
        self.entries
            .iter()
            .flatten()
            .find(|p| matches!(p.partition_type, 0x0b | 0x0c))
            .copied()
    }
}

/// Exclusive parent owner exposing only a checked partition-relative extent.
pub struct PartitionDevice {
    device: Box<dyn BlockDevice>,
    entry: PartitionEntry,
}

impl PartitionDevice {
    /// Wrap a parent, checking the extent even for manually constructed entries.
    /// No allocation or I/O; ordinary-thread only because failure drops parent.
    /// # Arguments
    /// * `device` - Parent owner, consumed on both success and failure.
    /// * `entry` - Nonempty primary partition extent to expose.
    /// # Returns
    /// A new owner with capacity exactly `entry.sectors`.
    /// # Errors
    /// Returns [`BlockError::OutOfBounds`] for zero start/length, overflow, or an
    /// extent beyond the parent's capacity. No I/O is submitted.
    pub fn new(device: Box<dyn BlockDevice>, entry: PartitionEntry) -> Result<Self, BlockError> {
        if entry.start_lba == 0
            || entry.sectors == 0
            || entry
                .start_lba
                .checked_add(entry.sectors)
                .is_none_or(|end| end > device.capacity_sectors())
        {
            return Err(BlockError::OutOfBounds);
        }
        Ok(Self { device, entry })
    }

    fn translate(&self, sector: u64, bytes: usize) -> Result<u64, BlockError> {
        validate_transfer(self.entry.sectors, sector, bytes)?;
        self.entry
            .start_lba
            .checked_add(sector)
            .ok_or(BlockError::OutOfBounds)
    }
}

impl BlockDevice for PartitionDevice {
    fn capacity_sectors(&self) -> u64 {
        self.entry.sectors
    }

    fn is_read_only(&self) -> bool {
        self.device.is_read_only()
    }

    fn read_sectors(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        let absolute = self.translate(sector, buffer.len())?;
        if buffer.is_empty() {
            return Ok(());
        }
        self.device.read_sectors(absolute, buffer)
    }

    fn write_sectors(&mut self, sector: u64, buffer: &[u8]) -> Result<(), BlockError> {
        let absolute = self.translate(sector, buffer.len())?;
        if buffer.is_empty() {
            return Ok(());
        }
        self.device.write_sectors(absolute, buffer)
    }

    fn sync(&mut self) -> Result<(), BlockError> {
        self.device.sync()
    }
}
