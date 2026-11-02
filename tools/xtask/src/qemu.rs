use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

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
    /// Optional pre-existing raw disk; never implicitly formatted.
    pub(crate) disk: Option<PathBuf>,
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
            disk: artifacts.disk().map(Path::to_path_buf),
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
        append_disk(&mut command, self.disk.as_deref());
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

/// Validate a user-supplied raw disk before building or starting QEMU.
/// # Arguments
/// * `path` - Existing regular image file; symlinks resolve to their target.
/// # Returns
/// Canonical UTF-8 path. No formatting, partitioning, truncation or writes occur.
/// # Errors
/// Rejects nonexistent/nonregular/non-UTF8 files and non-sector-sized images.
pub(crate) fn validate_disk(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("invalid disk {}", path.display()))?;
    let metadata = path.metadata()?;
    if !metadata.is_file()
        || metadata.len() < 512
        || metadata.len() % 512 != 0
        || path.to_str().is_none()
    {
        bail!("--disk requires an existing sector-aligned regular UTF-8 raw image");
    }
    Ok(path)
}

/// Append exactly the same modern virtio-mmio block attachment to dump and run.
/// # Arguments
/// * `command` - QEMU command being constructed.
/// * `disk` - Validated raw image; none leaves diskless arguments unchanged.
/// # Returns
/// Nothing. JSON filenames prevent commas/options in paths becoming QEMU syntax.
pub(crate) fn append_disk(command: &mut Command, disk: Option<&Path>) {
    if let Some(disk) = disk {
        command.args(["-global", "virtio-mmio.force-legacy=false"]);
        command.arg("-blockdev").arg(
            serde_json::json!({
                "driver": "file", "node-name": "genrt-file", "filename": disk,
            })
            .to_string(),
        );
        command.arg("-blockdev").arg(
            serde_json::json!({
                "driver": "raw", "node-name": "genrt-disk", "file": "genrt-file",
            })
            .to_string(),
        );
        command.args(["-device", "virtio-blk-device,drive=genrt-disk"]);
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
    fn disk_dump_and_launch_share_json_attachment_and_diskless_stays_unchanged() {
        let disk = PathBuf::from("/tmp/disk,unsafe=on with 'quotes'.raw");
        let plain = Aarch64Artifacts::for_cpu_count(Profile::Debug, 4);
        let attached = plain.clone().with_disk(Some(disk.clone()));
        assert_ne!(plain.dtb(), attached.dtb());
        let normal = Config::from_artifacts(&plain, PathBuf::from("init.cpio"));
        assert!(!normal.command().get_args().any(|arg| arg == "-blockdev"));
        let config = Config::from_artifacts(&attached, PathBuf::from("init.cpio"));
        let mut dump = Command::new("qemu-system-aarch64");
        append_disk(&mut dump, attached.disk());
        let expected = dump.get_args().collect::<Vec<_>>();
        let launched = config.command();
        let actual = launched.get_args().collect::<Vec<_>>();
        assert!(actual.ends_with(&expected));
        let filename: serde_json::Value =
            serde_json::from_str(expected[3].to_str().unwrap()).unwrap();
        assert_eq!(filename["filename"], disk.to_str().unwrap());
        assert_eq!(filename["driver"], "file");
    }

    #[test]
    fn disk_validation_never_creates_or_truncates_input() {
        let root = std::env::temp_dir().join(format!("genrt-raw-disk-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(validate_disk(&root).is_err());
        let path = root.join("disk.raw");
        assert!(validate_disk(&path).is_err());
        let original = [0x71; 512];
        std::fs::write(&path, original).unwrap();
        assert_eq!(validate_disk(&path).unwrap(), path.canonicalize().unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::write(&path, [0; 513]).unwrap();
        assert!(validate_disk(&path).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 513);
        std::fs::remove_dir_all(root).unwrap();
    }

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
