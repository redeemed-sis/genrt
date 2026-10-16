use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Rust target triple for the supported AArch64 platform.
pub(crate) const AARCH64_TARGET: &str = "aarch64-unknown-none-softfloat";

const TEST_ARTIFACT_MARKERS: &[&[u8]] =
    &[b"GENRT_TEST_ARTIFACT_V1", b".genrt.test_marker", b"GTRT/1|"];
const PROFILE_MARKER: &[u8] = b"GENRT_WAKE_PROFILE_V1";

/// Verify that a kernel ELF belongs to the production feature composition.
///
/// # Arguments
///
/// * `path` - Linked kernel ELF to inspect.
///
/// # Returns
///
/// Returns success when no retained test marker, marker section name, or test
/// protocol implementation is present.
///
/// # Errors
///
/// Returns an error when the ELF cannot be read or contains test/diagnostic bytes.
pub(crate) fn verify_production_kernel(path: &Path) -> Result<()> {
    verify_kernel_composition(path, false)
}

/// Check a linked non-test kernel against its diagnostic selection.
///
/// # Arguments
///
/// * `path` - Linked ELF to read and inspect.
/// * `wake_profile` - Whether the retained profiler marker must be present.
///
/// # Returns
///
/// Returns success only for matching diagnostic identity and no test markers.
///
/// # Errors
///
/// Returns an error for filesystem failures, test bytes, or marker mismatch.
pub(crate) fn verify_kernel_composition(path: &Path, wake_profile: bool) -> Result<()> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read production kernel {}", path.display()))?;
    for marker in TEST_ARTIFACT_MARKERS {
        if bytes.windows(marker.len()).any(|window| window == *marker) {
            bail!(
                "production kernel {} contains test marker {:?}",
                path.display(),
                String::from_utf8_lossy(marker)
            );
        }
    }
    let profiled = bytes
        .windows(PROFILE_MARKER.len())
        .any(|w| w == PROFILE_MARKER);
    if profiled != wake_profile {
        bail!(
            "kernel {} has unexpected wake-profile identity",
            path.display()
        );
    }
    Ok(())
}

/// Cargo build profile used by host orchestration.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Profile {
    /// Unoptimized development artifacts with debug information.
    Debug,
    /// Optimized artifacts intended for distribution.
    Release,
}

impl Profile {
    /// Return Cargo's directory name for this profile.
    ///
    /// The returned value is either debug or release and requires no
    /// allocation.
    pub(crate) const fn dir_name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    /// Add this profile's selection flags to a Cargo command.
    ///
    /// # Arguments
    ///
    /// * command - Cargo command being assembled.
    ///
    /// The debug profile adds no argument; release appends --release. This
    /// function has no return value.
    pub(crate) fn apply_to_cargo(self, command: &mut std::process::Command) {
        if self == Self::Release {
            command.arg("--release");
        }
    }
}

/// Canonical paths for one AArch64 build profile.
#[derive(Clone, Debug)]
pub(crate) struct Aarch64Artifacts {
    root: PathBuf,
    cpu_count: usize,
}

impl Aarch64Artifacts {
    /// Construct artifact paths below the repository's Cargo target directory.
    ///
    /// # Arguments
    ///
    /// * profile - Selects either the debug or release subtree.
    ///
    /// # Returns
    ///
    /// Returns a path bundle. No files are created by this operation.
    pub(crate) fn new(profile: Profile) -> Self {
        Self {
            root: PathBuf::from("target")
                .join(AARCH64_TARGET)
                .join(profile.dir_name()),
            cpu_count: 1,
        }
    }

    /// Construct artifact paths for one explicit QEMU CPU topology.
    ///
    /// # Arguments
    ///
    /// * `profile` - Selects either the debug or release subtree.
    /// * `cpu_count` - CPU count used for DTB generation and QEMU launch.
    ///
    /// # Returns
    ///
    /// Returns a path bundle whose DTB name is topology-specific. Validation
    /// of the bounded count remains the workflow caller's responsibility.
    pub(crate) fn for_cpu_count(profile: Profile, cpu_count: usize) -> Self {
        Self {
            root: PathBuf::from("target")
                .join(AARCH64_TARGET)
                .join(profile.dir_name()),
            cpu_count,
        }
    }

    /// Return the profile artifact directory without creating it.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Select isolated Cargo/ELF paths for diagnostic feature composition.
    ///
    /// # Arguments
    ///
    /// * `profile` - Cargo profile for the artifact directory.
    /// * `cpu_count` - Explicit topology; the caller validates its bounds.
    /// * `wake_profile` - Select a namespace separate from canonical artifacts.
    ///
    /// # Returns
    ///
    /// Returns artifact paths without I/O or topology validation.
    pub(crate) fn for_composition(profile: Profile, cpu_count: usize, wake_profile: bool) -> Self {
        let mut paths = Self::for_cpu_count(profile, cpu_count);
        if wake_profile {
            paths.root = PathBuf::from("target/wake-profile")
                .join(AARCH64_TARGET)
                .join(profile.dir_name());
        }
        paths
    }

    /// Return the final linked kernel ELF path.
    pub(crate) fn kernel_elf(&self) -> PathBuf {
        self.root.join("genrt-aarch64.elf")
    }

    /// Return the generated QEMU virt DTB path.
    ///
    /// # Returns
    ///
    /// Returns the legacy single-CPU filename for topology one and a
    /// CPU-count-qualified filename for multi-CPU topologies. No file is
    /// created or validated.
    pub(crate) fn dtb(&self) -> PathBuf {
        if self.cpu_count == 1 {
            self.root.join("qemu-virt.dtb")
        } else {
            self.root
                .join(format!("qemu-virt-smp{}.dtb", self.cpu_count))
        }
    }

    /// Return the CPU count represented by this artifact bundle's DTB.
    ///
    /// # Returns
    ///
    /// Returns the exact count selected when the bundle was constructed.
    pub(crate) const fn cpu_count(&self) -> usize {
        self.cpu_count
    }

    /// Return the default CPIO initramfs path.
    pub(crate) fn initramfs(&self) -> PathBuf {
        self.root.join("initramfs.cpio")
    }

    /// Return the temporary initramfs staging directory.
    pub(crate) fn initramfs_staging(&self) -> PathBuf {
        self.root.join("initramfs-root")
    }

    /// Return the ELF output path for a userspace program.
    ///
    /// # Arguments
    ///
    /// * name - Program basename without an extension.
    ///
    /// # Returns
    ///
    /// Returns a path below the profile's user artifact directory.
    pub(crate) fn user_elf(&self, name: &str) -> PathBuf {
        self.root.join("user").join(format!("{name}.elf"))
    }

    /// Return the Cargo-produced AArch64 static library path.
    pub(crate) fn staticlib(&self) -> PathBuf {
        self.root.join("libgenrt_arch_aarch64.a")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_paths_do_not_alias_canonical_artifacts() {
        for profile in [Profile::Debug, Profile::Release] {
            let normal = Aarch64Artifacts::for_cpu_count(profile, 4);
            let diagnostic = Aarch64Artifacts::for_composition(profile, 4, true);
            assert_ne!(normal.staticlib(), diagnostic.staticlib());
            assert_ne!(normal.kernel_elf(), diagnostic.kernel_elf());
            assert_ne!(normal.dtb(), diagnostic.dtb());
            assert_eq!(diagnostic.cpu_count(), 4);
        }
    }

    #[test]
    fn release_validation_rejects_diagnostics() {
        let root =
            std::env::temp_dir().join(format!("genrt-profile-marker-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let elf = root.join("kernel.elf");
        fs::write(&elf, PROFILE_MARKER).unwrap();
        assert!(verify_production_kernel(&elf).is_err());
        verify_kernel_composition(&elf, true).unwrap();
        fs::write(&elf, b"normal ELF fixture").unwrap();
        verify_production_kernel(&elf).unwrap();
        assert!(verify_kernel_composition(&elf, true).is_err());
        fs::write(&elf, b"GENRT_WAKE_PROFILE_V1 GTRT/1|").unwrap();
        assert!(verify_kernel_composition(&elf, true).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
