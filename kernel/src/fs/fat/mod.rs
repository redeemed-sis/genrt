//! Persistent FAT32 volume and canonical file handles, behind one logical lease.

mod io;
mod ops;
mod shared;
pub(super) use shared::SharedDisk;
#[cfg(test)]
mod tests;

use super::backend::*;
#[cfg(test)]
use crate::block::BlockDevice;
use crate::{
    block::{BlockError, PartitionEntry},
    errno::{self, Errno},
    sched::{self, WaitCause, WaitToken},
    sync::SpinLock,
};
use alloc::{boxed::Box, string::String, sync::Arc, vec, vec::Vec};
use core::{any::Any, task::Poll};

const MAX_NODES: usize = 4096;
const MAX_PATH: usize = 1024;
const MAX_DEPTH: usize = 32;
const ROOT: Node = Node {
    id: 1,
    kind: Kind::Directory,
};

enum Handle {
    Directory(u32),
    File(resident_fat::File),
}

struct Entry {
    path: String,
    handle: Handle,
}

impl Entry {
    fn kind(&self) -> Kind {
        match self.handle {
            Handle::Directory(_) => Kind::Directory,
            Handle::File(_) => Kind::File,
        }
    }
}

struct Volume {
    filesystem: resident_fat::FileSystem<io::Disk>,
    nodes: Vec<Entry>,
    failed: Option<Errno>,
}

// SAFETY: the only erased non-Send member in resident-fat 0.3.1 is its clock.
// This private owner is constructed only with the library's Send EpochClock;
// no custom clock is installed or exposed. Disk and canonical handles are Send.
// No references escape the exclusive lease; moving the owner never permits
// concurrent filesystem access. Re-audit this invariant on dependency updates.
unsafe impl Send for Volume {}

struct State {
    volume: Option<Box<Volume>>,
    waiters: [Option<WaitToken>; crate::config::KERNEL_THREAD_CAPACITY],
}

/// Mounted FAT32 instance with canonical handles and exclusive logical ownership.
/// Only lease transfer uses a lock; library allocations and I/O are task work.
pub(crate) struct Fat {
    state: SpinLock<State>,
}

impl Fat {
    /// Mount a partition once, retaining its FAT and directory caches.
    /// Allocation and bounded I/O occur in ordinary IRQ-enabled task context.
    /// # Arguments
    /// * `device` - Exclusive partition owner, consumed on success or failure.
    /// # Returns
    /// Instance with a stable root, suitable for boot namespace publication.
    /// # Errors
    /// `ENOTSUP` for unsupported format, sector geometry or FAT features;
    /// mapped media, allocation, geometry and device timeout errors otherwise.
    /// Never formats the device.
    #[cfg(test)]
    pub(crate) fn new(device: Box<dyn BlockDevice>) -> Result<Self, Errno> {
        Self::mount(io::Disk::new(device))
    }

    /// Mount a primary partition sharing a physical disk with other volumes.
    /// Allocates and performs bounded I/O in ordinary IRQ-enabled task context.
    /// # Arguments
    /// * `parent` - Physical reservation; no driver reference escapes its lease.
    /// * `entry` - Primary partition extent, revalidated before access.
    /// * `readonly` - Mount-level protection in addition to media protection.
    /// # Returns
    /// Resident filesystem with the physical owner returned to its reservation.
    /// # Errors
    /// Geometry/media errors as for mount; `EAGAIN` if the disk is reserved.
    /// Failed mounting returns the physical owner through adapter RAII.
    pub(crate) fn from_partition(
        parent: Arc<SharedDisk>,
        entry: PartitionEntry,
        readonly: bool,
    ) -> Result<Self, Errno> {
        Self::mount(io::Disk::partition(parent, entry, readonly)?)
    }

    fn mount(mut disk: io::Disk) -> Result<Self, Errno> {
        match disk.acquire(None) {
            Poll::Ready(result) => result?,
            Poll::Pending => return Err(errno::EAGAIN),
        }
        let mut boot = [0; 512];
        resident_fat::BlockDevice::read(&mut disk, 0, &mut boot).map_err(block_errno)?;
        validate_geometry(&boot)?;
        let mut filesystem = resident_fat::FileSystem::mount(disk).map_err(fat_errno)?;
        let root = filesystem.root_cluster();
        if let Some(waiters) = filesystem.device_mut().release() {
            waiters.wake();
        }
        Ok(Self {
            state: SpinLock::new(State {
                volume: Some(Box::new(Volume {
                    filesystem,
                    nodes: vec![Entry {
                        path: String::new(),
                        handle: Handle::Directory(root),
                    }],
                    failed: None,
                })),
                waiters: [None; crate::config::KERNEL_THREAD_CAPACITY],
            }),
        })
    }

    /// Set mount protection on the configuration volume before publication.
    /// # Arguments
    /// * `readonly` - Final fstab/fallback policy; physical protection persists.
    /// # Returns
    /// Success after an unlocked, non-I/O policy update; no allocation.
    /// # Errors
    /// `EAGAIN` if exclusive bootstrap ownership was violated.
    pub(super) fn set_read_only(&self, readonly: bool) -> Result<(), Errno> {
        let mut lease = self.acquire(None).map_err(|_| errno::EAGAIN)?;
        lease
            .volume
            .as_mut()
            .unwrap()
            .filesystem
            .device_mut()
            .set_read_only(readonly);
        Ok(())
    }

    fn acquire(&self, wake: Option<WaitToken>) -> Result<Lease<'_>, Poll<Result<Reply, Errno>>> {
        let volume = {
            let mut state = self.state.lock();
            match state.volume.take() {
                Some(volume) => volume,
                None => {
                    let Some(token) = wake else {
                        return Err(Poll::Ready(Err(errno::EAGAIN)));
                    };
                    state.waiters[token.thread().index()] = Some(token);
                    return Err(Poll::Pending);
                }
            }
        };
        let mut lease = Lease {
            owner: self,
            volume: Some(volume),
        };
        let result = lease
            .volume
            .as_mut()
            .unwrap()
            .filesystem
            .device_mut()
            .acquire(wake);
        match result {
            Poll::Ready(Ok(())) => Ok(lease),
            Poll::Ready(Err(error)) => Err(Poll::Ready(Err(error))),
            Poll::Pending => Err(Poll::Pending),
        }
    }
}

struct Lease<'a> {
    owner: &'a Fat,
    volume: Option<Box<Volume>>,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let disk_waiters = self
            .volume
            .as_mut()
            .unwrap()
            .filesystem
            .device_mut()
            .release();
        let waiters = {
            let mut state = self.owner.state.lock();
            assert!(state.volume.is_none());
            state.volume = self.volume.take();
            core::mem::replace(
                &mut state.waiters,
                [None; crate::config::KERNEL_THREAD_CAPACITY],
            )
        };
        if let Some(waiters) = disk_waiters {
            waiters.wake();
        }
        for token in waiters.into_iter().flatten() {
            let _ = sched::complete_wait(token, WaitCause::Notified);
        }
    }
}

impl Backend for Fat {
    fn root(&self) -> Node {
        ROOT
    }

    fn poll(
        &self,
        target: Node,
        request: &mut Request,
        wake: Option<WaitToken>,
    ) -> Poll<Result<Reply, Errno>> {
        let mut lease = match self.acquire(wake) {
            Ok(lease) => lease,
            Err(result) => return result,
        };
        Poll::Ready(lease.volume.as_mut().unwrap().operate(target, request))
    }

    fn close(&self, _node: Node, _state: Option<Box<dyn Any + Send>>) {
        // Canonical library handles belong to the mounted volume, not OFDs.
        // Their drop performs no I/O; durability is an explicit fsync operation.
    }
}

impl Volume {
    fn operate(&mut self, target: Node, request: &mut Request) -> Result<Reply, Errno> {
        if let Some(error) = self.failed.or(self.filesystem.device().failure) {
            return Err(error);
        }
        let result = ops::execute(&mut self.filesystem, &mut self.nodes, target, request);
        if matches!(result, Err(errno::EIO)) {
            self.failed = Some(errno::EIO);
        }
        if let Some(error) = self.filesystem.device().failure {
            self.failed = Some(error);
            return match result {
                Ok(reply @ Reply::Io { count: 1.., .. }) => Ok(reply),
                _ => Err(error),
            };
        }
        result
    }
}

// Cover gaps in resident-fat 0.3.1; delegate ordinary BPB validation to it.
fn validate_geometry(boot: &[u8; 512]) -> Result<(), Errno> {
    if u16::from_le_bytes(boot[22..24].try_into().unwrap()) == 0 {
        // BootSector::parse computes this u32 expression without checked math.
        let reserved = u32::from(u16::from_le_bytes(boot[14..16].try_into().unwrap()));
        let sectors_per_fat = u32::from_le_bytes(boot[36..40].try_into().unwrap());
        sectors_per_fat
            .checked_mul(u32::from(boot[16]))
            .and_then(|n| n.checked_add(reserved))
            .ok_or(errno::EINVAL)?;
    }
    let geometry =
        resident_fat::BootSector::parse(boot).map_err(|error| fat_errno(error.into()))?;
    if !geometry.is_valid_cluster(geometry.root_cluster)
        || geometry
            .fs_info_sector
            .is_some_and(|sector| sector >= geometry.reserved_sectors)
    {
        return Err(errno::EINVAL);
    }
    let flags = u16::from_le_bytes(boot[40..42].try_into().unwrap());
    // resident-fat 0.3.1 always reads FAT 0 and writes every copy. It cannot
    // handle active-table selection or an unknown FAT32 revision safely.
    if flags & 0x80 != 0 || boot[42..44] != [0, 0] {
        return Err(errno::ENOTSUP);
    }
    Ok(())
}

fn node(nodes: &[Entry], index: usize) -> Node {
    Node {
        id: index as u64 + 1,
        kind: nodes[index].kind(),
    }
}

fn register(nodes: &mut Vec<Entry>, path: String, handle: Handle) -> Result<Node, Errno> {
    if let Some(index) = nodes.iter().position(|entry| entry.path == path) {
        // Never replace the authoritative file with a fresh, possibly stale copy.
        return Ok(node(nodes, index));
    }
    if nodes.len() == MAX_NODES {
        return Err(errno::ENOMEM);
    }
    nodes.try_reserve(1).map_err(|_| errno::ENOMEM)?;
    let index = nodes.len();
    nodes.push(Entry { path, handle });
    Ok(node(nodes, index))
}

fn child_path(parent: &str, name: &str) -> Result<String, Errno> {
    let length = parent.len() + usize::from(!parent.is_empty()) + name.len();
    if name.is_empty()
        || name.len() > NAME_MAX
        || name.contains(['/', '\0', '\\'])
        || matches!(name, "." | "..")
    {
        return Err(errno::EINVAL);
    }
    if length > MAX_PATH || parent.bytes().filter(|b| *b == b'/').count() + 1 >= MAX_DEPTH {
        return Err(errno::ENAMETOOLONG);
    }
    let mut path = String::new();
    path.try_reserve_exact(length).map_err(|_| errno::ENOMEM)?;
    path.push_str(parent);
    if !parent.is_empty() {
        path.push('/');
    }
    path.push_str(name);
    Ok(path)
}

fn fat_errno(error: resident_fat::Error<BlockError>) -> Errno {
    use resident_fat::{BootError, Error, FatError, Geometry};
    match error {
        Error::Device(error) => block_errno(error),
        Error::NotFound { .. } => errno::ENOENT,
        Error::NotADirectory { .. } => errno::ENOTDIR,
        Error::IsADirectory { .. } => errno::EISDIR,
        Error::AlreadyExists { .. } => errno::EEXIST,
        Error::Fat(FatError::DiskFull { .. }) | Error::DirectoryFull => errno::ENOSPC,
        Error::OutOfMemory { .. } => errno::ENOMEM,
        Error::Boot(BootError::UnsupportedFormat(_))
        | Error::Boot(BootError::BadGeometry(Geometry::SectorSize(_))) => errno::ENOTSUP,
        Error::BadName { .. } | Error::Boot(_) => errno::EINVAL,
        _ => errno::EIO,
    }
}

/// Translate transport failures without exposing architecture details to VFS.
/// # Arguments
/// * `error` - Block operation failure.
/// # Returns
/// Stable positive errno; no allocation, I/O, locks or IRQ changes.
pub(super) fn block_errno(error: BlockError) -> Errno {
    match error {
        BlockError::ReadOnly => errno::EROFS,
        BlockError::Unsupported => errno::ENOTSUP,
        BlockError::Timeout => errno::ETIMEDOUT,
        BlockError::OutOfBounds | BlockError::InvalidBuffer | BlockError::TransferTooLarge => {
            errno::EINVAL
        }
        _ => errno::EIO,
    }
}
