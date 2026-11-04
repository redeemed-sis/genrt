pub mod backend;
pub(crate) mod call;
mod fat;
pub mod fd;
pub mod initramfs;
pub(crate) mod io;
mod mounts;
pub mod path;
pub mod ramfs;
#[cfg(any(test, feature = "qemu-test-vfs"))]
pub(crate) mod testfs;
#[cfg(test)]
mod tests;
pub mod vfs;

/// Construct and freeze the namespace in the first ordinary initialization task.
/// Storage discovery, fstab parsing and FAT I/O run with IRQs enabled, outside
/// shared locks. Initramfs stays root; the first disk's first primary FAT32
/// supplies `/fstab`, with `/mnt/disk` fallback only when that file is absent.
/// # Returns
/// Success after publishing initramfs and optional fstab/fallback FAT32 mounts.
/// # Errors
/// Returns config, filesystem or device errors, or `EBUSY` on duplicate
/// publication. No disk is a supported ramfs-only configuration. A present
/// empty config mounts nothing; malformed or unreadable config never falls back.
pub(crate) fn initialize_namespace() -> Result<(), crate::errno::Errno> {
    use crate::errno;
    unsafe extern "C" {
        fn arch_storage_init() -> i32;
    }
    let mut namespace = vfs::Namespace::new(ramfs::root_backend().ok_or(errno::EINVAL)?)?;
    // SAFETY: called once in ordinary initialization task context, after the
    // allocator, runtime mappings, CPU registry and local IRQ setup are live.
    let count = usize::try_from(unsafe { arch_storage_init() }).map_err(|_| errno::EIO)?;
    mounts::install(&mut namespace, count)?;
    #[cfg(feature = "qemu-test-vfs")]
    testfs::install(&mut namespace)?;
    namespace.publish()
}
