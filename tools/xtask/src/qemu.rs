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
/// Maximum QEMU CPUs supported by the kernel's fixed logical CPU storage.
pub(crate) const MAX_CPUS: usize = 4;

/// QEMU behavior when the guest requests a system reset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResetBehavior {
    /// Reset the virtual machine and begin a fresh guest boot.
    Reset,
    /// Exit QEMU so an unexpected reset fails a bounded contract run.
    Exit,
}

/// Complete input set for one AArch64 QEMU invocation.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    /// Final kernel ELF passed to QEMU.
    pub(crate) kernel: PathBuf,
    /// Platform DTB loaded into the boot-protocol slot.
    pub(crate) dtb: PathBuf,
    /// CPIO archive loaded into the reserved initramfs window.
    pub(crate) initramfs: PathBuf,
    /// Number of virtual CPUs represented by both QEMU and the loaded DTB.
    pub(crate) cpu_count: usize,
    /// Whether QEMU starts halted with a GDB server on the default port.
    pub(crate) wait_for_gdb: bool,
    /// QEMU action selected for a guest system reset.
    pub(crate) reset_behavior: ResetBehavior,
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
            cpu_count: artifacts.cpu_count(),
            wait_for_gdb: false,
            reset_behavior: ResetBehavior::Reset,
        }
    }

    /// Build the canonical QEMU command shared by run, debug, tests, and dist.
    ///
    /// # Returns
    ///
    /// Returns an unspawned command using the artifact CPU count for `-smp`.
    /// Callers choose inherited or piped stdio.
    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new("qemu-system-aarch64");
        command
            .args(["-machine", MACHINE, "-cpu", CPU, "-smp"])
            .arg(self.cpu_count.to_string())
            .args([
                "-display", "none", "-monitor", "none", "-nic", "none", "-serial", "stdio",
            ]);
        match self.reset_behavior {
            ResetBehavior::Reset => {
                command.args(["-action", "reboot=reset,shutdown=poweroff"]);
            }
            ResetBehavior::Exit => {
                command.arg("-no-reboot");
            }
        }
        command
            .arg("-kernel")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::Profile;

    #[test]
    fn command_uses_artifact_cpu_topology() {
        let artifacts = Aarch64Artifacts::for_cpu_count(Profile::Debug, 4);
        let config = Config::from_artifacts(&artifacts, PathBuf::from("initramfs.cpio"));
        let command = config.command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let smp = args
            .iter()
            .position(|arg| arg == "-smp")
            .expect("QEMU command must contain -smp");
        assert_eq!(args.get(smp + 1).map(String::as_str), Some("4"));
        assert!(!args.iter().any(|arg| arg == "-no-reboot"));
    }

    #[test]
    fn production_command_keeps_qemu_running_across_reset() {
        let artifacts = Aarch64Artifacts::for_cpu_count(Profile::Debug, 1);
        let mut config = Config::from_artifacts(&artifacts, PathBuf::from("initramfs.cpio"));
        config.reset_behavior = ResetBehavior::Reset;
        let command = config.command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(!args.iter().any(|arg| arg == "-no-reboot"));
        let action = args
            .iter()
            .position(|arg| arg == "-action")
            .expect("terminal QEMU command must contain -action");
        assert_eq!(
            args.get(action + 1).map(String::as_str),
            Some("reboot=reset,shutdown=poweroff")
        );
    }

    #[test]
    fn bounded_contract_can_exit_on_an_unexpected_reset() {
        let artifacts = Aarch64Artifacts::for_cpu_count(Profile::Debug, 1);
        let mut config = Config::from_artifacts(&artifacts, PathBuf::from("initramfs.cpio"));
        config.reset_behavior = ResetBehavior::Exit;
        let args = config
            .command()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg == "-no-reboot"));
        assert!(!args.iter().any(|arg| arg == "-action"));
    }
}
