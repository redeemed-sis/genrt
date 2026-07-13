use std::{path::PathBuf, process::Command};

use crate::artifacts::Aarch64Artifacts;

/// Canonical QEMU machine configuration used by all workflows.
pub(crate) const MACHINE: &str = "virt,gic-version=2";
/// Canonical emulated CPU model.
pub(crate) const CPU: &str = "cortex-a72";
/// Physical address of the QEMU-loaded DTB.
pub(crate) const DTB_LOAD_ADDR: &str = "0x40000000";
/// Physical address of the QEMU-loaded initramfs.
pub(crate) const INITRAMFS_LOAD_ADDR: &str = "0x47000000";

/// Complete input set for one AArch64 QEMU invocation.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    /// Final kernel ELF passed to QEMU.
    pub(crate) kernel: PathBuf,
    /// Platform DTB loaded into the boot-protocol slot.
    pub(crate) dtb: PathBuf,
    /// CPIO archive loaded into the reserved initramfs window.
    pub(crate) initramfs: PathBuf,
    /// Whether QEMU starts halted with a GDB server on the default port.
    pub(crate) wait_for_gdb: bool,
}

impl Config {
    /// Construct a QEMU configuration from canonical build artifacts.
    ///
    /// # Arguments
    ///
    /// * artifacts - Kernel and DTB paths for one build profile.
    /// * initramfs - Exact archive to pass to QEMU.
    ///
    /// # Returns
    ///
    /// Returns a non-debugging launch configuration.
    pub(crate) fn from_artifacts(artifacts: &Aarch64Artifacts, initramfs: PathBuf) -> Self {
        Self {
            kernel: artifacts.kernel_elf(),
            dtb: artifacts.dtb(),
            initramfs,
            wait_for_gdb: false,
        }
    }

    /// Build the canonical QEMU command shared by run, debug, tests, and dist.
    ///
    /// Returns an unspawned command. Callers choose inherited or piped stdio.
    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new("qemu-system-aarch64");
        command
            .args([
                "-machine",
                MACHINE,
                "-cpu",
                CPU,
                "-smp",
                "1",
                "-display",
                "none",
                "-monitor",
                "none",
                "-nic",
                "none",
                "-serial",
                "stdio",
                "-no-reboot",
                "-kernel",
            ])
            .arg(&self.kernel)
            .arg("-device")
            .arg(format!(
                "loader,file={},addr={DTB_LOAD_ADDR}",
                self.dtb.display()
            ))
            .arg("-device")
            .arg(format!(
                "loader,file={},addr={INITRAMFS_LOAD_ADDR},force-raw=on",
                self.initramfs.display()
            ));
        if self.wait_for_gdb {
            command.args(["-S", "-s"]);
        }
        command
    }

    /// Render the canonical command as a shell-readable multiline string.
    ///
    /// Returns display text only; this method does not invoke QEMU.
    pub(crate) fn display(&self) -> String {
        let command = self.command();
        let mut parts = Vec::new();
        parts.push(command.get_program().to_string_lossy().into_owned());
        parts.extend(
            command
                .get_args()
                .map(|arg| shell_quote(&arg.to_string_lossy())),
        );
        parts.join(" \\\n  ")
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-._/:,=".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
