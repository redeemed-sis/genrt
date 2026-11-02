use super::*;
use crate::block::{SECTOR_SIZE, validate_transfer};
use alloc::{format, string::ToString, sync::Arc};
use resident_fat::BlockDevice as _;
use std::sync::Mutex;

struct Memory {
    bytes: Arc<Mutex<Vec<u8>>>,
}

#[derive(Default)]
struct Faults {
    read: bool,
    read_error: Option<BlockError>,
    read_sector: Option<u64>,
    write_sector: Option<u64>,
    sync: bool,
    readonly: bool,
    sync_count: usize,
    boot_reads: usize,
    writes: usize,
    reads: usize,
}

struct FaultDevice {
    inner: Memory,
    faults: Arc<Mutex<Faults>>,
}

impl BlockDevice for FaultDevice {
    fn capacity_sectors(&self) -> u64 {
        self.inner.capacity_sectors()
    }
    fn is_read_only(&self) -> bool {
        self.faults.lock().unwrap().readonly
    }
    fn read_sectors(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        let mut faults = self.faults.lock().unwrap();
        faults.reads += 1;
        if let Some(error) = faults.read_error {
            return Err(error);
        }
        if faults.read
            || faults
                .read_sector
                .is_some_and(|at| sector <= at && at < sector + (buffer.len() / SECTOR_SIZE) as u64)
        {
            return Err(BlockError::Io);
        }
        if sector == 0 {
            faults.boot_reads += 1;
        }
        drop(faults);
        self.inner.read_sectors(sector, buffer)
    }
    fn write_sectors(&mut self, sector: u64, buffer: &[u8]) -> Result<(), BlockError> {
        let mut faults = self.faults.lock().unwrap();
        faults.writes += 1;
        if faults.readonly {
            return Err(BlockError::ReadOnly);
        }
        if faults.write_sector == Some(sector) {
            return Err(BlockError::Io);
        }
        drop(faults);
        self.inner.write_sectors(sector, buffer)
    }
    fn sync(&mut self) -> Result<(), BlockError> {
        let mut faults = self.faults.lock().unwrap();
        faults.sync_count += 1;
        if faults.sync {
            Err(BlockError::Io)
        } else {
            Ok(())
        }
    }
}

impl BlockDevice for Memory {
    fn capacity_sectors(&self) -> u64 {
        (self.bytes.lock().unwrap().len() / SECTOR_SIZE) as u64
    }
    fn read_sectors(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        validate_transfer(self.capacity_sectors(), sector, buffer.len())?;
        let start = sector as usize * SECTOR_SIZE;
        buffer.copy_from_slice(&self.bytes.lock().unwrap()[start..start + buffer.len()]);
        Ok(())
    }
    fn write_sectors(&mut self, sector: u64, buffer: &[u8]) -> Result<(), BlockError> {
        validate_transfer(self.capacity_sectors(), sector, buffer.len())?;
        let start = sector as usize * SECTOR_SIZE;
        self.bytes.lock().unwrap()[start..start + buffer.len()].copy_from_slice(buffer);
        Ok(())
    }
    fn sync(&mut self) -> Result<(), BlockError> {
        Ok(())
    }
}

fn fixture() -> (Fat, Arc<Mutex<Vec<u8>>>) {
    let bytes = Arc::new(Mutex::new(vec![0; 64 * 1024 * 1024]));
    let mut media = bytes.lock().unwrap();
    let mut cursor = fatfs::StdIoWrapper::new(std::io::Cursor::new(&mut *media));
    fatfs::format_volume(
        &mut cursor,
        fatfs::FormatVolumeOptions::new()
            .fat_type(fatfs::FatType::Fat32)
            .bytes_per_cluster(512),
    )
    .unwrap();
    drop(media);
    (
        Fat::new(Box::new(Memory {
            bytes: bytes.clone(),
        }))
        .unwrap(),
        bytes,
    )
}

fn poll(fs: &Fat, target: Node, request: &mut Request) -> Result<Reply, Errno> {
    match fs.poll(target, request, None) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("unexpected pending"),
    }
}

fn create(fs: &Fat, name: &[u8]) -> Node {
    match poll(
        fs,
        ROOT,
        &mut Request::new(Operation::Create(name.to_vec())),
    )
    .unwrap()
    {
        Reply::Node(node) => node,
        _ => panic!("not node"),
    }
}

#[test]
fn mountpoints_follow_fat_case_and_short_name_aliases() {
    use crate::fs::{
        ramfs::{MountedRamfs, RamFile},
        vfs::{Namespace, Resolver},
    };

    let (empty, bytes) = fixture();
    drop(empty);
    let (parent_alias, child_alias) = {
        let mut media = bytes.lock().unwrap();
        let volume = fatfs::FileSystem::new(
            fatfs::StdIoWrapper::new(std::io::Cursor::new(&mut *media)),
            fatfs::FsOptions::new(),
        )
        .unwrap();
        let root = volume.root_dir();
        let parent = root.create_dir("Parent Directory").unwrap();
        parent.create_dir("Mount Directory").unwrap();
        let alias = |directory: &fatfs::Dir<'_, _, _, _>, name: &str| {
            let entry = directory
                .iter()
                .map(Result::unwrap)
                .find(|entry| entry.eq_name(name))
                .unwrap();
            // These directory names have no extension in their 8.3 aliases.
            core::str::from_utf8(&entry.short_file_name_as_bytes()[..8])
                .unwrap()
                .trim_end()
                .to_string()
        };
        let aliases = (
            alias(&root, "Parent Directory"),
            alias(&parent, "Mount Directory"),
        );
        drop(parent);
        drop(root);
        volume.unmount().unwrap();
        aliases
    };
    let root = Arc::new(Fat::new(Box::new(Memory { bytes })).unwrap());
    let mounted = || {
        Arc::new(
            MountedRamfs::new(
                vec![RamFile {
                    path: b"/visible".to_vec(),
                    data: b"mounted partition",
                }],
                vec![],
            )
            .unwrap(),
        )
    };
    let mut namespace = Namespace::new(root).unwrap();
    namespace
        .mount(b"/Parent Directory/Mount Directory", mounted())
        .unwrap();
    for path in [
        "/parent directory/mount directory/visible".to_string(),
        format!("/{parent_alias}/{child_alias}/visible"),
    ] {
        let location = match Resolver::new(namespace.root(), path.as_bytes(), false)
            .unwrap()
            .poll(&namespace, None)
        {
            Poll::Ready(Ok(location)) => location,
            _ => panic!("mount alias did not reach the mounted filesystem: {path}"),
        };
        assert_eq!(location.executable(), Some(b"mounted partition".as_slice()));
    }
    assert_eq!(
        namespace.mount(b"/parent directory/mount directory", mounted()),
        Err(errno::EBUSY)
    );
    assert_eq!(
        namespace.mount(b"/parent directory", mounted()),
        Err(errno::EBUSY)
    );
}

#[test]
fn multicluster_reopen_append_truncate_and_small_library_allocations() {
    let (fs, bytes) = fixture();
    let node = create(&fs, b"long test filename.bin");
    let mut request = Request::new(Operation::Write {
        offset: 0,
        append: false,
    });
    request.buffer = vec![0x5a; 4096];
    let (_, counts) = crate::test_arch_stubs::allocations(|| {
        for index in 0..8 {
            request.operation = Operation::Write {
                offset: index * 4096,
                append: false,
            };
            assert!(matches!(
                poll(&fs, node, &mut request),
                Ok(Reply::Io { count: 4096, .. })
            ));
        }
        request.operation = Operation::Write {
            offset: 0,
            append: true,
        };
        assert!(matches!(
            poll(&fs, node, &mut request),
            Ok(Reply::Io {
                count: 4096,
                next: 36864
            })
        ));
        request.operation = Operation::Read {
            offset: 8192,
            limit: 4096,
        };
        request.buffer.fill(0);
        assert!(matches!(
            poll(&fs, node, &mut request),
            Ok(Reply::Io { count: 4096, .. })
        ));
        assert!(request.buffer.iter().all(|b| *b == 0x5a));
        request.operation = Operation::Truncate { size: 8197 };
        assert!(matches!(poll(&fs, node, &mut request), Ok(Reply::Done)));
        request.operation = Operation::Sync;
        assert!(matches!(poll(&fs, node, &mut request), Ok(Reply::Done)));
    });
    // FAT run/metadata vectors are now an explicit task-context exception.
    assert!(counts.0 > 0 && counts.0 < 2048, "{counts:?}");
    drop(fs);
    let reopened = Fat::new(Box::new(Memory { bytes })).unwrap();
    let mut lookup = Request::new(Operation::Lookup(b"LONG TEST FILENAME.BIN".to_vec()));
    let Reply::Node(node) = poll(&reopened, ROOT, &mut lookup).unwrap() else {
        panic!("node");
    };
    let Reply::Stat(stat) = poll(&reopened, node, &mut Request::new(Operation::Stat)).unwrap()
    else {
        panic!("stat");
    };
    assert_eq!(stat.size, 8197);
}

#[test]
fn partial_write_reports_committed_prefix_and_failed_volume_cannot_claim_sync() {
    let (fs, bytes) = fixture();
    let node = create(&fs, b"DATA.BIN");
    let mut request = Request::new(Operation::Write {
        offset: 0,
        append: false,
    });
    request.buffer = vec![0x31; 4096];
    poll(&fs, node, &mut request).unwrap();
    drop(fs);
    let sector = bytes
        .lock()
        .unwrap()
        .chunks_exact(512)
        .position(|b| b.iter().all(|b| *b == 0x31))
        .unwrap() as u64;
    let faults = Arc::new(Mutex::new(Faults::default()));
    let fs = Fat::new(Box::new(FaultDevice {
        inner: Memory {
            bytes: bytes.clone(),
        },
        faults: faults.clone(),
    }))
    .unwrap();
    let Reply::Node(node) = poll(
        &fs,
        ROOT,
        &mut Request::new(Operation::Lookup(b"DATA.BIN".to_vec())),
    )
    .unwrap() else {
        panic!("node");
    };
    faults.lock().unwrap().write_sector = Some(sector + 1);
    request.operation = Operation::Write {
        offset: 0,
        append: false,
    };
    request.buffer.resize(1024, 0);
    request.buffer.fill(0x72);
    let result = poll(&fs, node, &mut request);
    assert!(matches!(
        result,
        Ok(Reply::Io {
            count: 512,
            next: 512
        })
    ));
    assert!(matches!(
        poll(&fs, node, &mut Request::new(Operation::Sync)),
        Err(errno::EIO)
    ));
    let data = bytes.lock().unwrap();
    assert!(
        data[sector as usize * 512..(sector as usize + 1) * 512]
            .iter()
            .all(|b| *b == 0x72)
    );
    assert!(
        data[(sector as usize + 1) * 512..(sector as usize + 2) * 512]
            .iter()
            .all(|b| *b == 0x31)
    );
}

#[test]
fn readonly_mount_errors_sync_failure_and_geometry_are_explicit() {
    let (fs, bytes) = fixture();
    create(&fs, b"READ.BIN");
    drop(fs);
    let faults = Arc::new(Mutex::new(Faults {
        readonly: true,
        ..Faults::default()
    }));
    let device = || {
        Box::new(FaultDevice {
            inner: Memory {
                bytes: bytes.clone(),
            },
            faults: faults.clone(),
        }) as Box<dyn BlockDevice>
    };
    let fs = Fat::new(device()).unwrap();
    assert!(matches!(
        poll(
            &fs,
            ROOT,
            &mut Request::new(Operation::Create(b"NO.BIN".to_vec()))
        ),
        Err(errno::EROFS)
    ));
    let Reply::Node(node) = poll(
        &fs,
        ROOT,
        &mut Request::new(Operation::Lookup(b"READ.BIN".to_vec())),
    )
    .unwrap() else {
        panic!("node");
    };
    assert!(matches!(
        poll(
            &fs,
            node,
            &mut Request::new(Operation::Open(OpenFlags {
                write: true,
                ..OpenFlags::default()
            }))
        ),
        Err(errno::EROFS)
    ));
    assert!(matches!(
        poll(
            &fs,
            node,
            &mut Request::new(Operation::Truncate { size: 0 })
        ),
        Err(errno::EROFS)
    ));
    faults.lock().unwrap().sync = true;
    assert!(matches!(
        poll(&fs, node, &mut Request::new(Operation::Sync)),
        Ok(Reply::Done)
    ));
    assert_eq!(faults.lock().unwrap().sync_count, 0);
    assert_eq!(faults.lock().unwrap().writes, 0);
    drop(fs);
    faults.lock().unwrap().readonly = false;
    let fs = Fat::new(device()).unwrap();
    let Reply::Node(node) = poll(
        &fs,
        ROOT,
        &mut Request::new(Operation::Lookup(b"READ.BIN".to_vec())),
    )
    .unwrap() else {
        panic!("node")
    };
    assert!(matches!(
        poll(&fs, node, &mut Request::new(Operation::Sync)),
        Err(errno::EIO)
    ));
    assert_eq!(faults.lock().unwrap().sync_count, 1);
    faults.lock().unwrap().sync = false;
    assert!(matches!(
        poll(&fs, node, &mut Request::new(Operation::Sync)),
        Err(errno::EIO)
    ));
    assert_eq!(faults.lock().unwrap().sync_count, 1);
    faults.lock().unwrap().read = true;
    assert!(matches!(Fat::new(device()), Err(errno::EIO)));
    drop(fs);
    faults.lock().unwrap().read = false;
    bytes.lock().unwrap()[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(Fat::new(device()), Err(errno::EINVAL)));
    bytes.lock().unwrap()[510] = 0;
    assert!(matches!(Fat::new(device()), Err(errno::EINVAL)));
}

fn shared_fixture() -> (Arc<SharedDisk>, [PartitionEntry; 2], Arc<Mutex<Faults>>) {
    let (fs, bytes) = fixture();
    create(&fs, b"READ.BIN");
    poll(&fs, ROOT, &mut Request::new(Operation::Sync)).unwrap();
    drop(fs);
    let sectors = {
        let mut media = bytes.lock().unwrap();
        let length = media.len();
        media.extend_from_within(..);
        media.resize(2 * length + 512, 0);
        media.copy_within(..2 * length, 512);
        media[..512].fill(0);
        (length / 512) as u64
    };
    let faults = Arc::new(Mutex::new(Faults::default()));
    let owner = Arc::new(SharedDisk::new(Box::new(FaultDevice {
        inner: Memory { bytes },
        faults: faults.clone(),
    })));
    let entry = |start_lba| PartitionEntry {
        bootable: false,
        partition_type: 0x0c,
        start_lba,
        sectors,
    };
    (owner, [entry(1), entry(1 + sectors)], faults)
}

#[test]
fn partition_volumes_wait_for_physical_owner_and_restore_both_leases() {
    let (owner, entries, _) = shared_fixture();
    let first = Fat::from_partition(owner.clone(), entries[0], false).unwrap();
    let second = Fat::from_partition(owner.clone(), entries[1], false).unwrap();
    let lease = first.acquire(None).ok().unwrap();
    let cpu = crate::cpu::CpuId::from_index(0).unwrap();
    let token = WaitToken::for_profile_test(sched::ThreadId::new(0, 1), cpu, 17);
    let mut request = Request::new(Operation::Lookup(b"READ.BIN".to_vec()));
    assert!(matches!(
        second.poll(ROOT, &mut request, Some(token)),
        Poll::Pending
    ));
    assert!(matches!(
        second.poll(ROOT, &mut request, None),
        Poll::Ready(Err(errno::EAGAIN))
    ));
    assert_eq!(owner.take_waiter(token.thread().index()), Some(token));
    drop(lease);
    assert!(matches!(
        poll(&second, ROOT, &mut request),
        Ok(Reply::Node(_))
    ));
    assert!(matches!(
        poll(&first, ROOT, &mut request),
        Ok(Reply::Node(_))
    ));
    // Cached, non-I/O operations also reserve the physical owner. A different
    // disk remains independently usable while this disk is leased.
    let (independent, _) = fixture();
    let lease = first.acquire(None).ok().unwrap();
    assert!(matches!(
        poll(
            &independent,
            ROOT,
            &mut Request::new(Operation::Open(OpenFlags::default()))
        ),
        Ok(Reply::Done)
    ));
    drop(lease);
    let device = match owner.acquire(None) {
        Poll::Ready(Ok(device)) => device,
        _ => panic!("physical owner lost"),
    };
    owner.restore(device).wake();
}

#[test]
fn failed_partition_mount_returns_owner_and_mount_readonly_never_writes() {
    let (owner, entries, faults) = shared_fixture();
    faults.lock().unwrap().read = true;
    assert!(matches!(
        Fat::from_partition(owner.clone(), entries[0], false),
        Err(errno::EIO)
    ));
    faults.lock().unwrap().read = false;
    let readonly = Fat::from_partition(owner.clone(), entries[0], true).unwrap();
    let writable = Fat::from_partition(owner, entries[1], false).unwrap();
    let mut lookup = Request::new(Operation::Lookup(b"READ.BIN".to_vec()));
    let Reply::Node(node) = poll(&readonly, ROOT, &mut lookup).unwrap() else {
        panic!("node")
    };
    for operation in [
        Operation::Open(OpenFlags {
            write: true,
            ..OpenFlags::default()
        }),
        Operation::Truncate { size: 0 },
        Operation::Write {
            offset: 0,
            append: false,
        },
    ] {
        assert!(matches!(
            poll(&readonly, node, &mut Request::new(operation)),
            Err(errno::EROFS)
        ));
    }
    assert!(matches!(
        poll(
            &readonly,
            ROOT,
            &mut Request::new(Operation::Create(b"READ.BIN".to_vec()))
        ),
        Err(errno::EROFS)
    ));
    assert!(matches!(
        poll(&readonly, node, &mut Request::new(Operation::Sync)),
        Ok(Reply::Done)
    ));
    assert_eq!(faults.lock().unwrap().writes, 0);
    assert_eq!(faults.lock().unwrap().sync_count, 0);
    create(&writable, b"OTHER.BIN");
    poll(&writable, ROOT, &mut Request::new(Operation::Sync)).unwrap();
    assert!(faults.lock().unwrap().writes > 0);
}

#[test]
fn sector_bounds_remain_checked_without_a_fat_operation_budget() {
    let bytes = Arc::new(Mutex::new(vec![0; 4096]));
    let mut disk = io::Disk::new(Box::new(Memory { bytes }));
    assert_eq!(disk.read(8, &mut [0; 512]), Err(BlockError::OutOfBounds));
    assert_eq!(disk.read(0, &mut [0; 1]), Err(BlockError::InvalidBuffer));
    assert_eq!(
        disk.read(0, &mut vec![0; 129 * 512]),
        Err(BlockError::TransferTooLarge)
    );
    assert_eq!(disk.read(8, &mut []), Ok(()));
    // Cross the removed global call limit without resetting adapter state.
    let mut buffer = [0; 512];
    for _ in 0..40_000 {
        disk.read(0, &mut buffer).unwrap();
    }
    disk.write(0, &buffer).unwrap();
    disk.sync().unwrap();
    assert_eq!(disk.failure, None);
}

#[test]
fn device_timeouts_and_io_errors_still_latch_without_resubmission() {
    for error in [BlockError::Timeout, BlockError::Io] {
        let faults = Arc::new(Mutex::new(Faults {
            read_error: Some(error),
            ..Faults::default()
        }));
        let mut disk = io::Disk::new(Box::new(FaultDevice {
            inner: Memory {
                bytes: Arc::new(Mutex::new(vec![0; 4096])),
            },
            faults: faults.clone(),
        }));
        let mut buffer = [0; 512];
        assert_eq!(disk.read(0, &mut buffer), Err(error));
        assert_eq!(disk.failure, Some(block_errno(error)));
        faults.lock().unwrap().read_error = None;
        let expected = if error == BlockError::Timeout {
            BlockError::Timeout
        } else {
            BlockError::Poisoned
        };
        assert_eq!(disk.read(0, &mut buffer), Err(expected));
        assert_eq!(disk.write(0, &buffer), Err(expected));
        assert_eq!(disk.sync(), Err(expected));
        let calls = faults.lock().unwrap();
        assert_eq!((calls.reads, calls.writes, calls.sync_count), (1, 0, 0));
    }
}

#[test]
fn overflowing_geometry_is_rejected_without_writing() {
    let (fs, bytes) = fixture();
    drop(fs);
    bytes.lock().unwrap()[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
    let faults = Arc::new(Mutex::new(Faults::default()));
    assert!(matches!(
        Fat::new(Box::new(FaultDevice {
            inner: Memory { bytes },
            faults: faults.clone(),
        })),
        Err(errno::EINVAL)
    ));
    assert_eq!(faults.lock().unwrap().writes, 0);
}

#[test]
fn canonical_files_share_metadata_not_positions_and_io_never_remounts() {
    let (initial, bytes) = fixture();
    drop(initial);
    let faults = Arc::new(Mutex::new(Faults::default()));
    let fs = Fat::new(Box::new(FaultDevice {
        inner: Memory {
            bytes: bytes.clone(),
        },
        faults: faults.clone(),
    }))
    .unwrap();
    let node = create(&fs, b"Shared long filename.bin");
    let mut lookup = Request::new(Operation::Lookup(b"SHARED LONG FILENAME.BIN".to_vec()));
    let Reply::Node(alias) = poll(&fs, ROOT, &mut lookup).unwrap() else {
        panic!("node");
    };
    assert_eq!(alias, node);
    let mut lease = fs.acquire(None).ok().unwrap();
    let volume = lease.volume.as_mut().unwrap();
    let root = volume.filesystem.root_cluster();
    let short_name = volume
        .filesystem
        .read_dir(root)
        .unwrap()
        .get("Shared long filename.bin")
        .unwrap()
        .short_name()
        .to_display_string();
    drop(lease);
    let Reply::Node(short_alias) = poll(
        &fs,
        ROOT,
        &mut Request::new(Operation::Lookup(short_name.into_bytes())),
    )
    .unwrap() else {
        panic!("node");
    };
    assert_eq!(short_alias, node);
    let mut first = Request::new(Operation::Open(OpenFlags::default()));
    let mut second = Request::new(Operation::Open(OpenFlags::default()));
    poll(&fs, node, &mut first).unwrap();
    poll(&fs, alias, &mut second).unwrap();
    first.operation = Operation::Write {
        offset: 0,
        append: false,
    };
    first.buffer = vec![b'A'; 1024];
    poll(&fs, node, &mut first).unwrap();
    second.operation = Operation::Write {
        offset: 1024,
        append: false,
    };
    second.buffer = vec![b'B'; 1024];
    poll(&fs, alias, &mut second).unwrap();
    first.operation = Operation::Write {
        offset: 0,
        append: false,
    };
    first.buffer = vec![b'C'; 1];
    poll(&fs, node, &mut first).unwrap();
    let Reply::Stat(stat) = poll(&fs, alias, &mut Request::new(Operation::Stat)).unwrap() else {
        panic!("stat");
    };
    assert_eq!(stat.size, 2048);
    // Creating a sibling invalidates the directory cache, not the canonical file.
    create(&fs, b"OTHER.BIN");
    first.operation = Operation::Sync;
    poll(&fs, node, &mut first).unwrap();
    let boot_reads = faults.lock().unwrap().boot_reads;
    second.operation = Operation::Read {
        offset: 1024,
        limit: 32,
    };
    second.buffer = vec![0; 32];
    for _ in 0..16 {
        assert!(matches!(
            poll(&fs, alias, &mut second),
            Ok(Reply::Io {
                count: 32,
                next: 1056
            })
        ));
        assert!(second.buffer.iter().all(|b| *b == b'B'));
    }
    assert_eq!(faults.lock().unwrap().boot_reads, boot_reads);
    let lease = fs.acquire(None).ok().unwrap();
    let expected_free = lease
        .volume
        .as_ref()
        .unwrap()
        .filesystem
        .fat()
        .free_clusters();
    drop(lease);
    let media = bytes.lock().unwrap();
    let info_sector = u16::from_le_bytes(media[48..50].try_into().unwrap()) as usize;
    let info = &media[info_sector * 512..][..512];
    assert_eq!(
        u32::from_le_bytes(info[488..492].try_into().unwrap()),
        expected_free
    );
}

#[test]
fn file_growth_zero_fills_gaps_and_close_does_no_device_work() {
    let (fs, bytes) = fixture();
    let node = create(&fs, b"GAP.BIN");
    let mut request = Request::new(Operation::Write {
        offset: 513,
        append: false,
    });
    request.buffer = vec![0x55; 2];
    assert!(matches!(
        poll(&fs, node, &mut request),
        Ok(Reply::Io {
            count: 2,
            next: 515
        })
    ));
    request.operation = Operation::Read {
        offset: 0,
        limit: 515,
    };
    request.buffer.resize(515, 0xff);
    poll(&fs, node, &mut request).unwrap();
    assert!(request.buffer[..513].iter().all(|b| *b == 0));
    assert_eq!(&request.buffer[513..], &[0x55; 2]);
    request.operation = Operation::Truncate { size: 1025 };
    poll(&fs, node, &mut request).unwrap();
    request.operation = Operation::Read {
        offset: 515,
        limit: 510,
    };
    poll(&fs, node, &mut request).unwrap();
    assert!(request.buffer[..510].iter().all(|b| *b == 0));
    // No implicit sync or device access on close, even with unsynced metadata.
    let media = bytes.lock().unwrap();
    fs.close(node, None);
    drop(media);
    assert!(matches!(
        poll(&fs, node, &mut Request::new(Operation::Sync)),
        Ok(Reply::Done)
    ));
    assert!(matches!(
        poll(
            &fs,
            ROOT,
            &mut Request::new(Operation::Create(b"bad/name".to_vec()))
        ),
        Err(errno::EINVAL)
    ));
}

#[test]
fn partial_read_returns_only_confirmed_fragments_and_latches_failure() {
    let (initial, bytes) = fixture();
    let node = create(&initial, b"READ.BIN");
    let mut request = Request::new(Operation::Write {
        offset: 0,
        append: false,
    });
    request.buffer = vec![0x47; 1024];
    poll(&initial, node, &mut request).unwrap();
    poll(&initial, node, &mut Request::new(Operation::Sync)).unwrap();
    drop(initial);
    let sector = bytes
        .lock()
        .unwrap()
        .chunks_exact(512)
        .position(|b| b.iter().all(|b| *b == 0x47))
        .unwrap() as u64;
    let faults = Arc::new(Mutex::new(Faults::default()));
    let fs = Fat::new(Box::new(FaultDevice {
        inner: Memory { bytes },
        faults: faults.clone(),
    }))
    .unwrap();
    let Reply::Node(node) = poll(
        &fs,
        ROOT,
        &mut Request::new(Operation::Lookup(b"READ.BIN".to_vec())),
    )
    .unwrap() else {
        panic!("node");
    };
    faults.lock().unwrap().read_sector = Some(sector + 1);
    request.operation = Operation::Read {
        offset: 0,
        limit: 1024,
    };
    request.buffer.fill(0);
    assert!(matches!(
        poll(&fs, node, &mut request),
        Ok(Reply::Io {
            count: 512,
            next: 512
        })
    ));
    assert!(request.buffer[..512].iter().all(|b| *b == 0x47));
    assert!(request.buffer[512..].iter().all(|b| *b == 0));
    assert!(matches!(
        poll(&fs, node, &mut Request::new(Operation::Sync)),
        Err(errno::EIO)
    ));
}

#[test]
fn metadata_failure_after_payload_never_claims_or_replays_the_failed_fragment() {
    let (initial, bytes) = fixture();
    drop(initial);
    let faults = Arc::new(Mutex::new(Faults::default()));
    let fs = Fat::new(Box::new(FaultDevice {
        inner: Memory {
            bytes: bytes.clone(),
        },
        faults: faults.clone(),
    }))
    .unwrap();
    let node = create(&fs, b"META.BIN");
    let mut request = Request::new(Operation::Write {
        offset: 0,
        append: false,
    });
    request.buffer = vec![0x31; 512];
    poll(&fs, node, &mut request).unwrap();
    poll(&fs, node, &mut Request::new(Operation::Sync)).unwrap();
    let directory_sector = bytes
        .lock()
        .unwrap()
        .chunks_exact(512)
        .position(|b| b.windows(11).any(|name| name == b"META    BIN"))
        .unwrap() as u64;
    faults.lock().unwrap().write_sector = Some(directory_sector);
    request.operation = Operation::Write {
        offset: 512,
        append: false,
    };
    request.buffer.fill(0x72);
    assert!(matches!(poll(&fs, node, &mut request), Err(errno::EIO)));
    assert!(
        bytes
            .lock()
            .unwrap()
            .chunks_exact(512)
            .any(|b| b.iter().all(|b| *b == 0x72))
    );
    faults.lock().unwrap().write_sector = None;
    assert!(matches!(poll(&fs, node, &mut request), Err(errno::EIO)));
    assert!(matches!(
        poll(&fs, node, &mut Request::new(Operation::Sync)),
        Err(errno::EIO)
    ));
}

#[test]
fn non_ascii_oem_short_names_are_not_silently_reinterpreted() {
    let (fs, bytes) = fixture();
    let node = create(&fs, b"LEGACY.BIN");
    poll(&fs, node, &mut Request::new(Operation::Sync)).unwrap();
    drop(fs);
    let mut media = bytes.lock().unwrap();
    let at = media.windows(11).position(|b| b == b"LEGACY  BIN").unwrap();
    media[at] = 0x82;
    drop(media);
    let fs = Fat::new(Box::new(Memory { bytes })).unwrap();
    assert!(matches!(
        poll(
            &fs,
            ROOT,
            &mut Request::new(Operation::Readdir { offset: 0 })
        ),
        Err(errno::ENOTSUP)
    ));
}

#[test]
fn disk_full_is_reported_without_poisoning_unchanged_canonical_metadata() {
    let (fs, _) = fixture();
    let node = create(&fs, b"FULL.BIN");
    assert!(matches!(
        poll(
            &fs,
            node,
            &mut Request::new(Operation::Truncate {
                size: u32::MAX as u64
            })
        ),
        Err(errno::ENOSPC)
    ));
    let mut request = Request::new(Operation::Write {
        offset: 0,
        append: false,
    });
    request.buffer = vec![0x44; 512];
    assert!(matches!(
        poll(&fs, node, &mut request),
        Ok(Reply::Io {
            count: 512,
            next: 512
        })
    ));
    assert!(matches!(
        poll(&fs, node, &mut Request::new(Operation::Sync)),
        Ok(Reply::Done)
    ));
}

#[test]
fn nonmirrored_active_fat_is_rejected_before_any_mutation() {
    let (fs, bytes) = fixture();
    drop(fs);
    let mut media = bytes.lock().unwrap();
    let reserved = u16::from_le_bytes(media[14..16].try_into().unwrap()) as usize;
    let fat_sectors = u32::from_le_bytes(media[36..40].try_into().unwrap()) as usize;
    assert_eq!(media[16], 2);
    let first = reserved * 512 + 8;
    let second = (reserved + fat_sectors) * 512 + 8;
    assert_ne!(&media[second..second + 4], &[0; 4]);
    media[first..first + 4].fill(0);
    media[40..42].copy_from_slice(&0x81u16.to_le_bytes());
    drop(media);
    let faults = Arc::new(Mutex::new(Faults::default()));
    assert!(matches!(
        Fat::new(Box::new(FaultDevice {
            inner: Memory {
                bytes: bytes.clone()
            },
            faults: faults.clone(),
        })),
        Err(errno::ENOTSUP)
    ));
    assert_eq!(faults.lock().unwrap().writes, 0);
    let media = bytes.lock().unwrap();
    assert_eq!(&media[first..first + 4], &[0; 4]);
    assert_ne!(&media[second..second + 4], &[0; 4]);
}
