//! Stable disk selectors and owned, cancellation-safe runtime mount calls.

use super::{
    fat::{Fat, SharedDisk},
    vfs::{Location, Namespace, Resolver},
};
use crate::{
    block::{self, Mbr, PartitionEntry},
    errno::{self, Errno},
    sched::WaitToken,
    sync::{OncePublication, SpinLock},
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::task::Poll;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Source {
    disk: usize,
    slot: usize,
}

struct Registry {
    disks: [Option<Arc<SharedDisk>>; block::MAX_BOOT_DEVICES],
    claimed: SpinLock<[[bool; 4]; block::MAX_BOOT_DEVICES]>,
}

static DISKS: OncePublication<Arc<Registry>> = OncePublication::new();

impl Registry {
    fn new(
        devices: [Option<Box<dyn block::BlockDevice>>; block::MAX_BOOT_DEVICES],
        count: usize,
    ) -> Result<Arc<Self>, Errno> {
        if count > block::MAX_BOOT_DEVICES
            || devices.iter().take(count).any(Option::is_none)
            || devices.iter().skip(count).any(Option::is_some)
        {
            return Err(errno::EIO);
        }
        Ok(Arc::new(Self {
            disks: devices.map(|device| device.map(|device| Arc::new(SharedDisk::new(device)))),
            claimed: SpinLock::new([[false; 4]; block::MAX_BOOT_DEVICES]),
        }))
    }

    fn reserve(self: &Arc<Self>, source: Source) -> Result<Reservation, Errno> {
        if self.disks[source.disk].is_none() {
            return Err(errno::ENOENT);
        }
        {
            let mut claimed = self.claimed.lock();
            let slot = &mut claimed[source.disk][source.slot];
            if *slot {
                return Err(errno::EBUSY);
            }
            *slot = true;
        }
        Ok(Reservation {
            registry: self.clone(),
            source,
            mounted: false,
        })
    }
}

struct Reservation {
    registry: Arc<Registry>,
    source: Source,
    mounted: bool,
}

impl Reservation {
    fn disk(&self) -> &Arc<SharedDisk> {
        self.registry.disks[self.source.disk].as_ref().unwrap()
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.mounted {
            self.registry.claimed.lock()[self.source.disk][self.source.slot] = false;
        }
    }
}

/// Retain discovered physical owners without reading partitions or mount policy.
/// Task/bootstrap only; allocation and device property callbacks are unlocked.
/// # Arguments
/// * `count` - Discovery-order count matching the architecture's staging registry.
/// # Returns
/// Success after publishing stable disk identities, including an empty registry.
/// # Errors
/// `EIO` for a staging mismatch; `EBUSY` for duplicate registry publication.
pub(super) fn register(count: usize) -> Result<(), Errno> {
    let registry = Registry::new(block::take_boot_devices(), count)?;
    DISKS.publish(registry).map_err(|_| errno::EBUSY)
}

/// Owned runtime mount state, retained by the process over exact disk/path waits.
/// No borrowed userspace memory or live exception context is retained.
pub(super) struct MountCall {
    // Field drop order is deliberate: unpublished filesystem/cache destruction
    // must finish before another caller can reserve this source.
    resolver: Option<Resolver>,
    filesystem: Option<Arc<Fat>>,
    reservation: Reservation,
    entry: Option<PartitionEntry>,
    target: Vec<u8>,
    readonly: bool,
    waiting: Option<WaitToken>,
}

impl MountCall {
    /// Validate copied mount arguments and exclusively reserve the source.
    /// Allocates in task context, without I/O or holding namespace/disk locks.
    /// # Arguments
    /// * `source` - Logical `/dev/diskNpM` selector, not a VFS path.
    /// * `target` - Absolute existing-directory path, resolved during polling.
    /// * `fstype` - `vfat` or `fat32` only.
    /// * `flags` - Zero or one (`MS_RDONLY`).
    /// * `data` - Must be zero; no option payload is supported.
    /// # Returns
    /// Owned continuation with source reserved before any FAT cache exists.
    /// # Errors
    /// `EINVAL` for arguments/selectors, `ENOTSUP` for filesystem type,
    /// `ENOENT` for an absent registry/disk, `EBUSY` for a claimed source,
    /// and resolver path-validation errors.
    pub(super) fn new(
        source: &[u8],
        target: &[u8],
        fstype: &[u8],
        flags: usize,
        data: usize,
    ) -> Result<Self, Errno> {
        Self::with_registry(
            DISKS.get().ok_or(errno::ENOENT)?.clone(),
            source,
            target,
            fstype,
            flags,
            data,
        )
    }

    fn with_registry(
        registry: Arc<Registry>,
        source: &[u8],
        target: &[u8],
        fstype: &[u8],
        flags: usize,
        data: usize,
    ) -> Result<Self, Errno> {
        if flags > 1 || data != 0 || !target.starts_with(b"/") || target.contains(&0) {
            return Err(errno::EINVAL);
        }
        if target.len() > crate::config::GENRT_PATH_MAX {
            return Err(errno::ENAMETOOLONG);
        }
        if !matches!(fstype, b"vfat" | b"fat32") {
            return Err(errno::ENOTSUP);
        }
        let reservation = registry.reserve(parse_source(source)?)?;
        Ok(Self {
            resolver: None,
            filesystem: None,
            reservation,
            entry: None,
            target: target.to_vec(),
            readonly: flags == 1,
            waiting: None,
        })
    }

    /// Advance disk admission, filesystem construction, target lookup and commit.
    /// All I/O, traversal, allocation and callbacks run in ordinary task context
    /// outside locks. Physical ownership is returned before target traversal.
    /// # Arguments
    /// * `namespace` - Global namespace to attach to; root cannot be replaced.
    /// * `wake` - Exact prepared token for physical contention or backend lookup.
    /// # Returns
    /// Success only after atomic publication, or `Pending` with owned progress.
    /// Never returns `Pending` after commit. A stale topology re-resolves the
    /// target from root while retaining the original resident FAT instance.
    /// # Errors
    /// MBR/partition, filesystem, resolver and mount conflict/capacity errors.
    /// `EAGAIN` on physical contention only if `wake` is absent. Drop completed
    /// or failed calls in task context to release reservations and staging.
    pub(super) fn poll(
        &mut self,
        namespace: &Namespace,
        wake: Option<WaitToken>,
    ) -> Poll<Result<(), Errno>> {
        if let Some(token) = self.waiting.take() {
            self.reservation.disk().cancel_wait(token);
        }
        let result = self.advance(namespace, wake);
        if result.is_pending() {
            self.waiting = wake;
        }
        result
    }

    fn advance(
        &mut self,
        namespace: &Namespace,
        wake: Option<WaitToken>,
    ) -> Poll<Result<(), Errno>> {
        if self.entry.is_none() {
            let disk = self.reservation.disk();
            let mut device = match disk.acquire(wake) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result?,
            };
            let table = Mbr::read(&mut *device).map_err(super::fat::block_errno);
            disk.restore(device).wake();
            let entry = table?.entries()[self.reservation.source.slot].ok_or(errno::ENOENT)?;
            if !matches!(entry.partition_type, 0x0b | 0x0c) {
                return Poll::Ready(Err(errno::ENOTSUP));
            }
            self.entry = Some(entry);
        }
        if self.filesystem.is_none() {
            let filesystem = match Fat::poll_from_partition(
                self.reservation.disk().clone(),
                self.entry.unwrap(),
                self.readonly,
                wake,
            ) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result?,
            };
            self.filesystem = Some(Arc::new(filesystem));
        }
        loop {
            if self.resolver.is_none() {
                self.resolver = Some(Resolver::new(namespace.root(), &self.target, false)?);
            }
            let location: Location = match self.resolver.as_mut().unwrap().poll(namespace, wake) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result?,
            };
            if namespace.attach(location, self.filesystem.as_ref().unwrap().clone())? {
                // Infallible transition: source was already reserved, and the
                // namespace now pins the filesystem. No waits follow commit.
                self.reservation.mounted = true;
                return Poll::Ready(Ok(()));
            }
            self.resolver = None;
        }
    }
}

impl Drop for MountCall {
    fn drop(&mut self) {
        if let Some(token) = self.waiting.take() {
            self.reservation.disk().cancel_wait(token);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        block::{BlockDevice, BlockError},
        fs::{backend::*, ramfs::MountedRamfs},
    };
    use alloc::vec;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use fatfs::Write;
    use std::sync::Mutex;

    struct Memory {
        bytes: Arc<Mutex<Vec<u8>>>,
        reads: Arc<AtomicUsize>,
    }

    impl BlockDevice for Memory {
        fn capacity_sectors(&self) -> u64 {
            (self.bytes.lock().unwrap().len() / 512) as u64
        }
        fn read_sectors(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
            block::validate_transfer(self.capacity_sectors(), sector, buffer.len())?;
            self.reads.fetch_add(1, Ordering::Relaxed);
            let start = sector as usize * 512;
            buffer.copy_from_slice(&self.bytes.lock().unwrap()[start..start + buffer.len()]);
            Ok(())
        }
        fn write_sectors(&mut self, sector: u64, buffer: &[u8]) -> Result<(), BlockError> {
            block::validate_transfer(self.capacity_sectors(), sector, buffer.len())?;
            let start = sector as usize * 512;
            self.bytes.lock().unwrap()[start..start + buffer.len()].copy_from_slice(buffer);
            Ok(())
        }
        fn sync(&mut self) -> Result<(), BlockError> {
            Ok(())
        }
    }

    fn registry(bytes: Vec<u8>) -> (Arc<Registry>, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let mut devices: [Option<Box<dyn BlockDevice>>; block::MAX_BOOT_DEVICES] =
            [const { None }; block::MAX_BOOT_DEVICES];
        devices[0] = Some(Box::new(Memory {
            bytes: Arc::new(Mutex::new(bytes)),
            reads: reads.clone(),
        }));
        (Registry::new(devices, 1).unwrap(), reads)
    }

    fn image() -> Vec<u8> {
        let mut bytes = vec![0; 512];
        bytes[510..].copy_from_slice(&[0x55, 0xaa]);
        for (slot, marker) in [(1, b"first".as_slice()), (3, b"second".as_slice())] {
            let mut volume = vec![0; 40 * 1024 * 1024];
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
                    // This is ordinary payload now, never a kernel boot policy.
                    root.create_file("fstab")
                        .unwrap()
                        .write_all(b"invalid kernel policy")
                        .unwrap();
                }
                fs.unmount().unwrap();
            }
            let offset = 446 + slot * 16;
            let start = (bytes.len() / 512) as u32;
            bytes[offset + 4] = 0x0c;
            bytes[offset + 8..offset + 12].copy_from_slice(&start.to_le_bytes());
            bytes[offset + 12..offset + 16]
                .copy_from_slice(&((volume.len() / 512) as u32).to_le_bytes());
            bytes.extend_from_slice(&volume);
        }
        bytes
    }

    fn namespace() -> Namespace {
        Namespace::new(Arc::new(
            MountedRamfs::new(
                Vec::new(),
                vec![b"/mnt/disk/EXTRA".to_vec(), b"/mnt/other".to_vec()],
            )
            .unwrap(),
        ))
        .unwrap()
    }

    fn call(
        registry: &Arc<Registry>,
        source: &[u8],
        target: &[u8],
        flags: usize,
    ) -> Result<MountCall, Errno> {
        MountCall::with_registry(registry.clone(), source, target, b"vfat", flags, 0)
    }

    fn resolve(ns: &Namespace, path: &[u8]) -> Result<Location, Errno> {
        match Resolver::new(ns.root(), path, false)?.poll(ns, None) {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("unexpected pending lookup"),
        }
    }

    fn read(ns: &Namespace, path: &[u8]) -> Vec<u8> {
        let location = resolve(ns, path).unwrap();
        let mut request = Request::new(Operation::Read {
            offset: 0,
            limit: 64,
        });
        request.buffer = vec![0; 64];
        let Poll::Ready(Ok(Reply::Io { count, .. })) = location.poll(&mut request, None) else {
            panic!("read")
        };
        request.buffer.truncate(count);
        request.buffer
    }

    #[test]
    fn registry_is_lazy_and_source_validation_and_reservation_precede_io() {
        let (registry, reads) = registry(vec![0; 512]);
        let ns = namespace();
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        assert!(matches!(
            resolve(&ns, b"/mnt/disk/MARKER"),
            Err(errno::ENOENT)
        ));
        for source in [
            b"/dev/disk0p0".as_slice(),
            b"/dev/disk0p5",
            b"/dev/disk32p1",
            b"/dev/disk0p1x",
            b"/dev/diskp1",
        ] {
            assert!(matches!(
                call(&registry, source, b"/mnt/disk", 0),
                Err(errno::EINVAL)
            ));
        }
        assert!(matches!(
            call(&registry, b"/dev/disk1p1", b"/mnt/disk", 0),
            Err(errno::ENOENT)
        ));
        assert!(matches!(
            call(&registry, b"/dev/disk0p1", b"relative", 0),
            Err(errno::EINVAL)
        ));
        assert!(matches!(
            call(&registry, b"/dev/disk0p1", b"/mnt/disk", 2),
            Err(errno::EINVAL)
        ));
        assert!(matches!(
            MountCall::with_registry(
                registry.clone(),
                b"/dev/disk0p1",
                b"/mnt/disk",
                b"ext4",
                0,
                0
            ),
            Err(errno::ENOTSUP)
        ));
        assert!(matches!(
            MountCall::with_registry(
                registry.clone(),
                b"/dev/disk0p1",
                b"/mnt/disk",
                b"vfat",
                0,
                1
            ),
            Err(errno::EINVAL)
        ));
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let reserve = || {
                let result = call(&registry, b"/dev/disk0p1", b"/mnt/disk", 0);
                barrier.wait();
                result
            };
            let first = scope.spawn(reserve);
            let second = scope.spawn(reserve);
            let first = first.join().unwrap();
            let second = second.join().unwrap();
            assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        });
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        let mut failed = call(&registry, b"/dev/disk0p1", b"/mnt/disk", 0).unwrap();
        assert!(matches!(
            failed.poll(&ns, None),
            Poll::Ready(Err(errno::EIO))
        ));
        drop(failed);
        assert!(call(&registry, b"/dev/disk0p1", b"/mnt/disk", 0).is_ok());
        assert_eq!(ns.mount_count(), 0);
    }

    #[test]
    fn runtime_mount_selects_original_slots_and_rolls_back_conflicts_and_failures() {
        let (registry, _) = registry(image());
        let ns = namespace();
        let mut empty = call(&registry, b"/dev/disk0p1", b"/mnt/disk", 0).unwrap();
        assert!(matches!(
            empty.poll(&ns, None),
            Poll::Ready(Err(errno::ENOENT))
        ));
        drop(empty);
        let mut failed = call(&registry, b"/dev/disk0p2", b"/missing", 0).unwrap();
        assert!(matches!(
            failed.poll(&ns, None),
            Poll::Ready(Err(errno::ENOENT))
        ));
        drop(failed);
        assert_eq!(ns.mount_count(), 0);
        let mut first = call(&registry, b"/dev/disk0p2", b"/mnt/disk", 0).unwrap();
        assert!(matches!(
            call(&registry, b"/dev/disk00p02", b"/mnt/other", 1),
            Err(errno::EBUSY)
        ));
        assert_eq!(first.poll(&ns, None), Poll::Ready(Ok(())));
        drop(first);
        assert!(matches!(
            call(&registry, b"/dev/disk0p2", b"/mnt/other", 0),
            Err(errno::EBUSY)
        ));
        assert_eq!(read(&ns, b"/mnt/disk/MARKER"), b"first");
        assert_eq!(read(&ns, b"/mnt/disk/fstab"), b"invalid kernel policy");
        let mut conflict = call(&registry, b"/dev/disk0p4", b"/mnt/disk", 0).unwrap();
        assert_eq!(conflict.poll(&ns, None), Poll::Ready(Err(errno::EBUSY)));
        drop(conflict);
        let mut second = call(&registry, b"/dev/disk0p4", b"/mnt/disk/EXTRA", 1).unwrap();
        assert_eq!(second.poll(&ns, None), Poll::Ready(Ok(())));
        drop(second);
        assert_eq!(read(&ns, b"/mnt/disk/EXTRA/MARKER"), b"second");
        let node = resolve(&ns, b"/mnt/disk/EXTRA/MARKER").unwrap();
        assert!(matches!(
            node.poll(&mut Request::new(Operation::Truncate { size: 0 }), None),
            Poll::Ready(Err(errno::EROFS))
        ));
        let node = resolve(&ns, b"/mnt/disk/MARKER").unwrap();
        assert!(matches!(
            node.poll(&mut Request::new(Operation::Truncate { size: 0 }), None),
            Poll::Ready(Ok(Reply::Done))
        ));
    }

    #[test]
    fn stale_target_is_reresolved_without_reconstructing_the_reserved_filesystem() {
        let bytes = image();
        let table =
            Mbr::parse(bytes[..512].try_into().unwrap(), (bytes.len() / 512) as u64).unwrap();
        let (registry, _) = registry(bytes);
        let ns = namespace();
        let mut child = call(&registry, b"/dev/disk0p4", b"/mnt/disk/EXTRA", 1).unwrap();
        child.entry = table.entries()[3];
        child.filesystem = Some(Arc::new(
            match Fat::poll_from_partition(
                child.reservation.disk().clone(),
                child.entry.unwrap(),
                true,
                None,
            ) {
                Poll::Ready(Ok(fs)) => fs,
                _ => panic!("filesystem construction"),
            },
        ));
        let original = child.filesystem.as_ref().unwrap().clone();
        let mut resolver = Resolver::new(ns.root(), &child.target, false).unwrap();
        assert!(matches!(resolver.poll(&ns, None), Poll::Ready(Ok(_))));
        child.resolver = Some(resolver);
        let mut parent = call(&registry, b"/dev/disk0p2", b"/mnt/disk", 0).unwrap();
        assert_eq!(parent.poll(&ns, None), Poll::Ready(Ok(())));
        assert_eq!(child.poll(&ns, None), Poll::Ready(Ok(())));
        assert!(Arc::ptr_eq(&original, child.filesystem.as_ref().unwrap()));
        assert_eq!(read(&ns, b"/mnt/disk/EXTRA/MARKER"), b"second");
        assert_eq!(
            resolve(&ns, b"/mnt/disk/EXTRA/..").unwrap().path(),
            b"/mnt/disk"
        );
    }

    #[test]
    fn disk_contention_during_mbr_and_fat_construction_uses_exact_cancellable_waits() {
        let bytes = image();
        let table =
            Mbr::parse(bytes[..512].try_into().unwrap(), (bytes.len() / 512) as u64).unwrap();
        let (registry, reads) = registry(bytes);
        let ns = namespace();
        let disk = registry.disks[0].as_ref().unwrap();
        let cpu = crate::cpu::CpuId::from_index(0).unwrap();
        let token = WaitToken::for_profile_test(crate::sched::ThreadId::new(0, 1), cpu, 71);
        for entry in [None, table.entries()[1]] {
            let device = match disk.acquire(None) {
                Poll::Ready(Ok(device)) => device,
                _ => panic!("owner"),
            };
            let mut pending = call(&registry, b"/dev/disk0p2", b"/mnt/disk", 0).unwrap();
            pending.entry = entry;
            assert_eq!(pending.poll(&ns, Some(token)), Poll::Pending);
            assert_eq!(reads.load(Ordering::Relaxed), 0);
            assert!(pending.filesystem.is_none());
            assert!(matches!(
                call(&registry, b"/dev/disk0p2", b"/mnt/other", 0),
                Err(errno::EBUSY)
            ));
            drop(pending);
            assert_eq!(disk.take_waiter(token.thread().index()), None);
            disk.restore(device).wake();
        }
        let mut resumed = call(&registry, b"/dev/disk0p2", b"/mnt/disk", 0).unwrap();
        let device = match disk.acquire(None) {
            Poll::Ready(Ok(device)) => device,
            _ => panic!("owner"),
        };
        assert_eq!(resumed.poll(&ns, Some(token)), Poll::Pending);
        assert_eq!(disk.take_waiter(token.thread().index()), Some(token));
        disk.restore(device).wake();
        assert_eq!(resumed.poll(&ns, Some(token)), Poll::Ready(Ok(())));
        assert_eq!(read(&ns, b"/mnt/disk/MARKER"), b"first");
    }
}
