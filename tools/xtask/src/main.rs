mod artifacts;
mod cli;
mod disk;
mod dist;
mod hardware;
mod initramfs;
mod product;
mod product_contract;
mod qemu;
mod test;
mod workflow;

// Host-compile the exact production sector/virtqueue sources without linking
// the kernel's bare-metal panic handler into this std executable.
#[cfg(test)]
extern crate alloc;
#[cfg(test)]
extern crate self as kernel;
#[cfg(test)]
#[path = "../../../kernel/src/block/mod.rs"]
pub mod block;
#[cfg(test)]
mod sync {
    pub struct SpinLock<T>(std::sync::Mutex<T>);
    impl<T> SpinLock<T> {
        pub const fn new(value: T) -> Self {
            Self(std::sync::Mutex::new(value))
        }
        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap()
        }
    }
}
#[cfg(test)]
#[path = "../../../kernel/src/dtb/runtime.rs"]
#[allow(dead_code)]
mod dtb;
#[cfg(test)]
#[path = "../../../arch/aarch64/src/platform/bootstrap/interrupts.rs"]
mod interrupts;
#[cfg(test)]
#[path = "../../../arch/aarch64/src/platform/bootstrap/virtio_mmio.rs"]
mod storage_discovery_checks;
#[cfg(test)]
#[path = "../../../arch/aarch64/src/platform/block/virtio_block/queue.rs"]
mod storage_driver_checks;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    workflow::dispatch(cli::Cli::parse())
}
