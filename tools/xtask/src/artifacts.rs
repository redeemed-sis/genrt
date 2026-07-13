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
/// Returns an error when the ELF cannot be read or contains test-only bytes.
pub(crate) fn verify_production_kernel(path: &Path) -> Result<()> {
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
        }
    }

    /// Return the profile artifact directory without creating it.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Return the final linked kernel ELF path.
    pub(crate) fn kernel_elf(&self) -> PathBuf {
        self.root.join("genrt-aarch64.elf")
    }

    /// Return the generated QEMU virt DTB path.
    pub(crate) fn dtb(&self) -> PathBuf {
        self.root.join("qemu-virt.dtb")
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
