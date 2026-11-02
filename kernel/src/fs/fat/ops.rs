//! Task-context operations on persistent, volume-owned canonical handles.

use super::*;
use crate::block::SECTOR_SIZE;

type Mounted = resident_fat::FileSystem<io::Disk>;

/// Complete an operation under exclusive logical volume ownership, unlocked.
/// # Arguments
/// * `fs` - Persistent filesystem; only its default epoch clock is permitted.
/// * `nodes` - Stable registry owning one authoritative file per node.
/// * `target` - Instance-local identity from the caller's pinned location.
/// * `request` - Reusable OFD staging and operation; positions remain VFS-owned.
/// # Returns
/// Completion, EOF or confirmed partial write progress. FAT may allocate small
/// metadata/run vectors in IRQ-enabled task context, never under shared locks.
/// # Errors
/// Mapped media, name, allocation, capacity and device timeout errors.
/// Failed library calls may have changed bytes; no failed call is replayed.
pub(super) fn execute(
    fs: &mut Mounted,
    nodes: &mut Vec<Entry>,
    target: Node,
    request: &mut Request,
) -> Result<Reply, Errno> {
    let index = usize::try_from(target.id.checked_sub(1).ok_or(errno::ENOENT)?)
        .map_err(|_| errno::ENOENT)?;
    let entry = nodes.get(index).ok_or(errno::ENOENT)?;
    if entry.kind() != target.kind {
        return Err(errno::EINVAL);
    }
    let readonly = fs.device().is_read_only();
    if readonly && matches!(request.operation, Operation::Create(_)) {
        return Err(errno::EROFS);
    }
    match &request.operation {
        Operation::Lookup(component) | Operation::Create(component) => {
            let Handle::Directory(cluster) = entry.handle else {
                return Err(errno::ENOTDIR);
            };
            let component = core::str::from_utf8(component).map_err(|_| errno::EINVAL)?;
            // Validate before giving a component to the library's path API.
            let path = child_path(&entry.path, component)?;
            let found = fs
                .read_dir(cluster)
                .map_err(fat_errno)?
                .get(component)
                .cloned();
            if let Some(found) = found {
                return register_found(fs, nodes, index, &found).map(Reply::Node);
            }
            if !matches!(request.operation, Operation::Create(_)) {
                return Err(errno::ENOENT);
            }
            if readonly {
                return Err(errno::EROFS);
            }
            if nodes.len() == MAX_NODES {
                return Err(errno::ENOMEM);
            }
            nodes.try_reserve(1).map_err(|_| errno::ENOMEM)?;
            let file = fs.create(&path).map_err(fat_errno)?;
            register(nodes, path, Handle::File(file)).map(Reply::Node)
        }
        Operation::Open(flags) => {
            if flags.write && readonly {
                return Err(errno::EROFS);
            }
            Ok(Reply::Done)
        }
        Operation::Sync => {
            // Readonly mounts never publish FAT/FSInfo or dirty-state metadata.
            if readonly {
                return Ok(Reply::Done);
            }
            fs.sync().map_err(fat_errno)?;
            fs.device_mut().sync().map_err(block_errno)?;
            Ok(Reply::Done)
        }
        Operation::Readdir { offset } => {
            let Handle::Directory(cluster) = entry.handle else {
                return Err(errno::ENOTDIR);
            };
            let offset = usize::try_from(*offset).map_err(|_| errno::EOVERFLOW)?;
            let found = fs
                .read_dir(cluster)
                .map_err(fat_errno)?
                .iter()
                .filter(|entry| !matches!(entry.name(), "." | ".."))
                .nth(offset)
                .cloned();
            let Some(found) = found else {
                return Ok(Reply::Entry(None));
            };
            let name = found.name().as_bytes().to_vec();
            let node = register_found(fs, nodes, index, &found)?;
            Ok(Reply::Entry(Some(DirEntry { name, node })))
        }
        _ => file_io(fs, target, &mut nodes[index].handle, request, readonly),
    }
}

fn register_found(
    fs: &mut Mounted,
    nodes: &mut Vec<Entry>,
    parent: usize,
    found: &resident_fat::DirEntry,
) -> Result<Node, Errno> {
    if found.long_name().is_none() && !found.short_name().as_bytes().is_ascii() {
        return Err(errno::ENOTSUP);
    }
    let path = child_path(&nodes[parent].path, found.name())?;
    if let Some(index) = nodes.iter().position(|entry| entry.path == path) {
        return Ok(node(nodes, index));
    }
    let handle = if found.is_directory() {
        Handle::Directory(found.first_cluster())
    } else {
        Handle::File(fs.open(&path).map_err(fat_errno)?)
    };
    register(nodes, path, handle)
}

fn file_io(
    fs: &mut Mounted,
    target: Node,
    handle: &mut Handle,
    request: &mut Request,
    readonly: bool,
) -> Result<Reply, Errno> {
    let Handle::File(file) = handle else {
        return Err(errno::EISDIR);
    };
    let size = u64::from(file.len());
    match &request.operation {
        Operation::Read { offset, limit } => {
            let count = (*limit).min(request.buffer.len());
            offset.checked_add(count as u64).ok_or(errno::EOVERFLOW)?;
            let mut done = 0;
            while done < count {
                let at = offset + done as u64;
                let take = (SECTOR_SIZE - at as usize % SECTOR_SIZE).min(count - done);
                let result = fs
                    .read_at(file, at, &mut request.buffer[done..done + take])
                    .map_err(fat_errno);
                match result {
                    Ok(0) => break,
                    Ok(bytes) => done += bytes,
                    Err(error) => {
                        if error == errno::EIO {
                            fs.device_mut().failure = Some(error);
                        }
                        if done == 0 {
                            return Err(error);
                        }
                        break;
                    }
                }
            }
            Ok(Reply::Io {
                count: done,
                next: offset.checked_add(done as u64).ok_or(errno::EOVERFLOW)?,
            })
        }
        Operation::Write { offset, append } => {
            if readonly {
                return Err(errno::EROFS);
            }
            let offset = if *append { size } else { *offset };
            if offset
                .checked_add(request.buffer.len() as u64)
                .is_none_or(|end| end > u32::MAX as u64)
            {
                return Err(errno::EOVERFLOW);
            }
            // The library has no partial-progress error. Submit one sector
            // fragment at a time and report only fully successful calls.
            // A failed fragment (including metadata publication) is uncertain;
            // the adapter latches device failure and never retries it.
            let mut done = 0;
            while done < request.buffer.len() {
                let at = offset + done as u64;
                let take =
                    (SECTOR_SIZE - at as usize % SECTOR_SIZE).min(request.buffer.len() - done);
                let result = fs
                    .write_at(file, at, &request.buffer[done..done + take])
                    .map_err(fat_errno);
                match result {
                    Ok(()) => done += take,
                    Err(error) => {
                        if error == errno::EIO {
                            fs.device_mut().failure = Some(error);
                        }
                        if done == 0 {
                            return Err(error);
                        }
                        break;
                    }
                }
            }
            Ok(Reply::Io {
                count: done,
                next: offset + done as u64,
            })
        }
        Operation::Truncate { size } => {
            if readonly {
                return Err(errno::EROFS);
            }
            let size = u32::try_from(*size).map_err(|_| errno::EOVERFLOW)?;
            fs.truncate(file, size).map_err(fat_errno)?;
            Ok(Reply::Done)
        }
        Operation::Stat => Ok(Reply::Stat(Stat { node: target, size })),
        Operation::Seek {
            current,
            displacement,
            whence,
        } => {
            let base = match whence {
                0 => 0,
                1 => *current,
                2 => size,
                _ => return Err(errno::EINVAL),
            };
            Ok(Reply::Position(
                base.checked_add_signed(*displacement)
                    .ok_or(errno::EINVAL)?,
            ))
        }
        _ => Err(errno::ENOTSUP),
    }
}
