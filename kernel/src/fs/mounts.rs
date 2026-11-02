//! Bounded boot-only fstab policy; source names select disks, not VFS devices.

use super::{
    backend::*,
    fat::{Fat, SharedDisk},
    vfs::Namespace,
};
use crate::{
    block::{self, Mbr},
    errno::{self, Errno},
};
use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::task::Poll;

const MAX_CONFIG: usize = 16 * 1024;
const MAX_ENTRIES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Source {
    disk: usize,
    slot: usize,
}

struct Entry {
    source: Source,
    path: Vec<u8>,
    readonly: bool,
    noauto: bool,
}

struct Disk {
    owner: Arc<SharedDisk>,
    table: Option<Mbr>,
}

impl Disk {
    fn table(&mut self) -> Result<Mbr, Errno> {
        if let Some(table) = self.table {
            return Ok(table);
        }
        let mut device = match self.owner.acquire(None) {
            Poll::Ready(result) => result?,
            Poll::Pending => return Err(errno::EAGAIN),
        };
        let result = Mbr::read(&mut *device).map_err(super::fat::block_errno);
        self.owner.restore(device).wake();
        let table = result?;
        self.table = Some(table);
        Ok(table)
    }
}

/// Consume firmware-ordered boot disks and apply optional partition-root fstab.
/// Ordinary exclusive init-task work: allocates, parses and performs bounded
/// I/O outside shared locks. Does not publish or replace the initramfs root.
/// # Arguments
/// * `namespace` - Unpublished namespace; existing mountpoint rules apply.
/// * `count` - Number reported by architecture discovery, at most 32.
/// # Returns
/// Ramfs-only for no disks; otherwise fstab mounts in line order, or the same
/// configuration filesystem at `/mnt/disk` only when `/fstab` is absent.
/// # Errors
/// Registry mismatch, first-disk MBR/FAT errors, config read/parse/resource
/// errors, invalid selected partitions, or mount errors. No scan fallback or
/// fallback for empty, malformed, unreadable or directory-valued fstab.
pub(super) fn install(namespace: &mut Namespace, count: usize) -> Result<(), Errno> {
    install_devices(namespace, block::take_boot_devices(), count)
}

fn install_devices(
    namespace: &mut Namespace,
    devices: [Option<Box<dyn block::BlockDevice>>; block::MAX_BOOT_DEVICES],
    count: usize,
) -> Result<(), Errno> {
    if count > block::MAX_BOOT_DEVICES
        || devices.iter().take(count).any(Option::is_none)
        || devices.iter().skip(count).any(Option::is_some)
    {
        return Err(errno::EIO);
    }
    let mut disks: Vec<Disk> = devices
        .into_iter()
        .flatten()
        .map(|device| Disk {
            owner: Arc::new(SharedDisk::new(device)),
            table: None,
        })
        .collect();
    let Some(first) = disks.first_mut() else {
        return Ok(());
    };
    let table = first.table()?;
    let slot = table
        .entries()
        .iter()
        .position(|entry| entry.is_some_and(|entry| matches!(entry.partition_type, 0x0b | 0x0c)))
        .ok_or(errno::ENOTSUP)?;
    let source = Source { disk: 0, slot };
    let config = Arc::new(Fat::from_partition(
        first.owner.clone(),
        table.entries()[slot].unwrap(),
        true,
    )?);
    let contents = match read_config(&config) {
        Ok(Some(contents)) => contents,
        Ok(None) => {
            config.set_read_only(false)?;
            return namespace.mount(b"/mnt/disk", config);
        }
        Err(error) => return Err(error),
    };
    let entries = parse(&contents)?;
    // Validate every selected partition before constructing another FAT cache.
    // Duplicate source identities were rejected during parsing, even noauto.
    let mut partitions = Vec::new();
    for entry in entries.iter().filter(|entry| !entry.noauto) {
        let disk = disks.get_mut(entry.source.disk).ok_or(errno::ENOENT)?;
        let partition = disk.table()?.entries()[entry.source.slot].ok_or(errno::ENOENT)?;
        if !matches!(partition.partition_type, 0x0b | 0x0c) {
            return Err(errno::ENOTSUP);
        }
        partitions.push(partition);
    }
    for (entry, partition) in entries.iter().filter(|entry| !entry.noauto).zip(partitions) {
        let filesystem = if entry.source == source {
            config.set_read_only(entry.readonly)?;
            config.clone()
        } else {
            Arc::new(Fat::from_partition(
                disks[entry.source.disk].owner.clone(),
                partition,
                entry.readonly,
            )?)
        };
        namespace.mount(&entry.path, filesystem)?;
    }
    Ok(())
}

fn complete(fs: &Fat, target: Node, request: &mut Request) -> Result<Reply, Errno> {
    match fs.poll(target, request, None) {
        Poll::Ready(result) => result,
        Poll::Pending => Err(errno::EAGAIN),
    }
}

fn read_config(fs: &Fat) -> Result<Option<Vec<u8>>, Errno> {
    let mut request = Request::new(Operation::Lookup(b"fstab".to_vec()));
    let node = match complete(fs, fs.root(), &mut request) {
        Ok(Reply::Node(node)) => node,
        Err(errno::ENOENT) => return Ok(None),
        Err(error) => return Err(error),
        _ => return Err(errno::EIO),
    };
    if node.kind != Kind::File {
        return Err(errno::EISDIR);
    }
    request.operation = Operation::Stat;
    let Reply::Stat(stat) = complete(fs, node, &mut request)? else {
        return Err(errno::EIO);
    };
    if stat.size > MAX_CONFIG as u64 {
        return Err(errno::EOVERFLOW);
    }
    // One extra byte distinguishes exactly-full input from an oversized file.
    request.buffer = vec![0; MAX_CONFIG + 1];
    let mut contents = Vec::new();
    loop {
        request.operation = Operation::Read {
            offset: contents.len() as u64,
            limit: MAX_CONFIG + 1 - contents.len(),
        };
        let Reply::Io { count, .. } = complete(fs, node, &mut request)? else {
            return Err(errno::EIO);
        };
        if count == 0 {
            return Ok(Some(contents));
        }
        if contents.len() + count > MAX_CONFIG {
            return Err(errno::EOVERFLOW);
        }
        contents.extend_from_slice(&request.buffer[..count]);
    }
}

fn parse(contents: &[u8]) -> Result<Vec<Entry>, Errno> {
    if contents.len() > MAX_CONFIG {
        return Err(errno::EOVERFLOW);
    }
    let mut entries: Vec<Entry> = Vec::new();
    for line in contents.split(|byte| *byte == b'\n') {
        let mut fields = line
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty())
            .take_while(|field| !field.starts_with(b"#"));
        let Some(source) = fields.next() else {
            continue;
        };
        let source = parse_source(&decode(source)?)?;
        let path = decode(fields.next().ok_or(errno::EINVAL)?)?;
        if !path.starts_with(b"/") || path.len() >= crate::config::GENRT_PATH_MAX {
            return Err(errno::EINVAL);
        }
        let kind = decode(fields.next().ok_or(errno::EINVAL)?)?;
        if !matches!(kind.as_slice(), b"vfat" | b"fat32") {
            return Err(errno::ENOTSUP);
        }
        let options = decode(fields.next().ok_or(errno::EINVAL)?)?;
        let mut readonly = false;
        let mut noauto = false;
        for option in options.split(|byte| *byte == b',') {
            match option {
                b"defaults" => {}
                b"rw" => readonly = false,
                b"ro" => readonly = true,
                b"noauto" => noauto = true,
                _ => return Err(errno::EINVAL),
            }
        }
        for _ in 0..2 {
            let numeric = decode(fields.next().ok_or(errno::EINVAL)?)?;
            if numeric.is_empty() || numeric.iter().any(|byte| *byte != b'0') {
                return Err(errno::EINVAL);
            }
        }
        if fields.next().is_some() || entries.iter().any(|entry| entry.source == source) {
            return Err(errno::EINVAL);
        }
        if entries.len() == MAX_ENTRIES {
            return Err(errno::ENOMEM);
        }
        entries.push(Entry {
            source,
            path,
            readonly,
            noauto,
        });
    }
    Ok(entries)
}

fn parse_source(source: &[u8]) -> Result<Source, Errno> {
    fn number(bytes: &[u8]) -> Result<usize, Errno> {
        if bytes.is_empty() {
            return Err(errno::EINVAL);
        }
        bytes.iter().try_fold(0usize, |n, byte| {
            if !byte.is_ascii_digit() {
                return Err(errno::EINVAL);
            }
            n.checked_mul(10)
                .and_then(|n| n.checked_add((byte - b'0') as usize))
                .ok_or(errno::EINVAL)
        })
    }
    let suffix = source.strip_prefix(b"/dev/disk").ok_or(errno::EINVAL)?;
    let separator = suffix
        .iter()
        .position(|byte| *byte == b'p')
        .ok_or(errno::EINVAL)?;
    let disk = number(&suffix[..separator])?;
    let partition = number(&suffix[separator + 1..])?;
    if disk >= block::MAX_BOOT_DEVICES || !(1..=4).contains(&partition) {
        return Err(errno::EINVAL);
    }
    Ok(Source {
        disk,
        slot: partition - 1,
    })
}

fn decode(field: &[u8]) -> Result<Vec<u8>, Errno> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < field.len() {
        let mut byte = field[index];
        if byte == b'\\' {
            let octal = field.get(index + 1..index + 4).ok_or(errno::EINVAL)?;
            if octal.iter().any(|byte| !(b'0'..=b'7').contains(byte)) {
                return Err(errno::EINVAL);
            }
            let value = octal
                .iter()
                .fold(0u16, |n, byte| n * 8 + u16::from(byte - b'0'));
            byte = u8::try_from(value).map_err(|_| errno::EINVAL)?;
            index += 3;
        }
        if byte == 0 {
            return Err(errno::EINVAL);
        }
        output.push(byte);
        index += 1;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::super::{
        ramfs::MountedRamfs,
        vfs::{Location, Resolver},
    };
    use super::*;
    use block::{BlockDevice, BlockError};
    use fatfs::Write;

    #[derive(Clone)]
    struct Memory(Vec<u8>);

    impl BlockDevice for Memory {
        fn capacity_sectors(&self) -> u64 {
            (self.0.len() / 512) as u64
        }
        fn read_sectors(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
            block::validate_transfer(self.capacity_sectors(), sector, buffer.len())?;
            let start = sector as usize * 512;
            buffer.copy_from_slice(&self.0[start..start + buffer.len()]);
            Ok(())
        }
        fn write_sectors(&mut self, sector: u64, buffer: &[u8]) -> Result<(), BlockError> {
            block::validate_transfer(self.capacity_sectors(), sector, buffer.len())?;
            let start = sector as usize * 512;
            self.0[start..start + buffer.len()].copy_from_slice(buffer);
            Ok(())
        }
        fn sync(&mut self) -> Result<(), BlockError> {
            Ok(())
        }
    }

    fn image(config: Option<&[u8]>, slot: usize, marker: &[u8]) -> Memory {
        let mut volume = vec![0; 64 * 1024 * 1024];
        {
            let mut io = fatfs::StdIoWrapper::new(std::io::Cursor::new(&mut volume));
            fatfs::format_volume(
                &mut io,
                fatfs::FormatVolumeOptions::new()
                    .fat_type(fatfs::FatType::Fat32)
                    .bytes_per_cluster(512),
            )
            .unwrap();
            let fs = fatfs::FileSystem::new(io, fatfs::FsOptions::new()).unwrap();
            {
                let root = fs.root_dir();
                root.create_dir("EXTRA").unwrap();
                root.create_file("MARKER")
                    .unwrap()
                    .write_all(marker)
                    .unwrap();
                if let Some(config) = config {
                    if config == b"directory" {
                        root.create_dir("fstab").unwrap();
                    } else {
                        root.create_file("fstab")
                            .unwrap()
                            .write_all(config)
                            .unwrap();
                    }
                }
            }
            fs.unmount().unwrap();
        }
        let mut bytes = vec![0; 512];
        bytes[510..].copy_from_slice(&[0x55, 0xaa]);
        let offset = 446 + slot * 16;
        bytes[offset + 4] = 0x0c;
        bytes[offset + 8..offset + 12].copy_from_slice(&1u32.to_le_bytes());
        bytes[offset + 12..offset + 16]
            .copy_from_slice(&((volume.len() / 512) as u32).to_le_bytes());
        bytes.extend_from_slice(&volume);
        Memory(bytes)
    }

    fn namespace() -> Namespace {
        Namespace::new(Arc::new(
            MountedRamfs::new(
                Vec::new(),
                vec![
                    b"/mnt".to_vec(),
                    b"/mnt/disk".to_vec(),
                    b"/mnt/other".to_vec(),
                ],
            )
            .unwrap(),
        ))
        .unwrap()
    }

    fn boot(namespace: &mut Namespace, disks: Vec<Memory>) -> Result<(), Errno> {
        let count = disks.len();
        let mut devices: [Option<Box<dyn BlockDevice>>; block::MAX_BOOT_DEVICES] =
            [const { None }; block::MAX_BOOT_DEVICES];
        for (slot, disk) in devices.iter_mut().zip(disks) {
            *slot = Some(Box::new(disk));
        }
        install_devices(namespace, devices, count)
    }

    fn resolve(namespace: &Namespace, path: &[u8]) -> Result<Location, Errno> {
        match Resolver::new(namespace.root(), path, false)?.poll(namespace, None) {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("exclusive boot traversal cannot wait"),
        }
    }

    fn read(namespace: &Namespace, path: &[u8]) -> Vec<u8> {
        let location = resolve(namespace, path).unwrap();
        let mut request = Request::new(Operation::Read {
            offset: 0,
            limit: 256,
        });
        request.buffer = vec![0; 256];
        let Poll::Ready(Ok(Reply::Io { count, .. })) = location.poll(&mut request, None) else {
            panic!("read")
        };
        request.buffer.truncate(count);
        request.buffer
    }

    #[test]
    fn boot_policy_distinguishes_absent_empty_invalid_and_first_disk_without_fat() {
        let mut ns = namespace();
        boot(&mut ns, Vec::new()).unwrap();
        assert!(resolve(&ns, b"/mnt/disk/MARKER").is_err());
        let mut ns = namespace();
        boot(&mut ns, vec![image(None, 2, b"fallback")]).unwrap();
        assert_eq!(read(&ns, b"/mnt/disk/MARKER"), b"fallback");
        assert!(resolve(&ns, b"/MARKER").is_err());
        let mut ns = namespace();
        boot(&mut ns, vec![image(Some(b" # empty\n"), 2, b"hidden")]).unwrap();
        assert!(resolve(&ns, b"/mnt/disk/MARKER").is_err());
        for config in [
            b"/dev/disk0p3 /mnt/disk vfat rw 0 0\ninvalid".as_slice(),
            b"directory",
        ] {
            let mut ns = namespace();
            assert!(boot(&mut ns, vec![image(Some(config), 2, b"hidden")]).is_err());
            assert!(resolve(&ns, b"/mnt/disk/MARKER").is_err());
        }
        let mut no_fat = image(None, 0, b"unused");
        no_fat.0[450] = 0x83;
        let mut ns = namespace();
        assert_eq!(
            boot(&mut ns, vec![no_fat, image(None, 0, b"never scanned")]),
            Err(errno::ENOTSUP)
        );
    }

    #[test]
    fn boot_policy_resolves_two_disks_actual_slots_nested_mounts_and_omitted_config_volume() {
        let config =
            b"/dev/disk0p2 /mnt/disk vfat rw 0 0\n/dev/disk1p3 /mnt/disk/EXTRA fat32 ro 0 0\n";
        let mut ns = namespace();
        boot(
            &mut ns,
            vec![image(Some(config), 1, b"first"), image(None, 2, b"second")],
        )
        .unwrap();
        assert_eq!(read(&ns, b"/mnt/disk/fstab"), config);
        assert_eq!(read(&ns, b"/mnt/disk/MARKER"), b"first");
        assert_eq!(read(&ns, b"/mnt/disk/EXTRA/MARKER"), b"second");
        let target = resolve(&ns, b"/mnt/disk/EXTRA/MARKER").unwrap();
        assert!(matches!(
            target.poll(&mut Request::new(Operation::Truncate { size: 0 }), None),
            Poll::Ready(Err(errno::EROFS))
        ));
        let mut ns = namespace();
        boot(
            &mut ns,
            vec![
                image(Some(b"/dev/disk1p3 /mnt/other vfat rw 0 0\n"), 1, b"hidden"),
                image(None, 2, b"second"),
            ],
        )
        .unwrap();
        assert_eq!(read(&ns, b"/mnt/other/MARKER"), b"second");
        assert!(resolve(&ns, b"/mnt/disk/MARKER").is_err());
    }

    #[test]
    fn fstab_parses_slots_options_comments_and_escaped_whitespace() {
        let entries = parse(b"# boot policy\n/dev/disk0p3 /mnt/my\\040disk vfat defaults,ro 0 00 # comment\n/dev/disk1p1 /mnt/other fat32 rw,noauto 0 0\n").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source, Source { disk: 0, slot: 2 });
        assert_eq!(entries[0].path, b"/mnt/my disk");
        assert!(entries[0].readonly);
        assert!(entries[1].noauto);
        assert!(!entries[1].readonly);
        assert!(parse(b" # empty\n").unwrap().is_empty());
    }

    #[test]
    fn fstab_rejects_invalid_tail_duplicates_and_resource_overflow() {
        for contents in [
            b"/dev/disk0p1 /mnt/disk vfat rw 0 0\ninvalid".as_slice(),
            b"/dev/disk0p1 /a vfat rw 0 0\n/dev/disk00p01 /b vfat ro 0 0",
            b"/dev/disk0p1 /a vfat rw 0 1",
            b"/dev/disk0p1 /a ext4 rw 0 0",
        ] {
            assert!(parse(contents).is_err());
        }
        assert!(parse(&vec![b' '; MAX_CONFIG + 1]).is_err());
        let mut contents = Vec::new();
        for disk in 0..17 {
            contents.extend_from_slice(
                alloc::format!("/dev/disk{disk}p1 /mnt/disk vfat noauto 0 0\n").as_bytes(),
            );
        }
        assert!(parse(&contents).is_err());
    }
}
