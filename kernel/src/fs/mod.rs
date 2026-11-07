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

/// Publish initramfs root and stable disks in the ordinary initialization task.
/// Discovery runs with IRQs enabled outside shared locks. No partition I/O,
/// filesystem construction or mount configuration parsing occurs here.
/// # Returns
/// Success with only initramfs root (plus feature-gated test fixtures).
/// # Errors
/// Returns root/device-registry errors or `EBUSY` on duplicate publication.
/// No disks is a supported root-only configuration.
pub(crate) fn initialize_namespace() -> Result<(), crate::errno::Errno> {
    use crate::errno;
    unsafe extern "C" {
        fn arch_storage_init() -> i32;
    }
    let namespace = vfs::Namespace::new(ramfs::root_backend().ok_or(errno::EINVAL)?)?;
    // SAFETY: called once in ordinary initialization task context, after the
    // allocator, runtime mappings, CPU registry and local IRQ setup are live.
    let count = usize::try_from(unsafe { arch_storage_init() }).map_err(|_| errno::EIO)?;
    mounts::register(count)?;
    #[cfg(feature = "qemu-test-vfs")]
    let namespace = {
        let mut namespace = namespace;
        testfs::install(&mut namespace)?;
        namespace
    };
    namespace.publish()
}
