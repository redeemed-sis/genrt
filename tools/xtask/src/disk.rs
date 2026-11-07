//! Explicit new-file provisioning, separate from attaching an existing disk.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const MIB: u64 = 1024 * 1024;
const SECTOR_SIZE: u64 = 512;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
struct DiskLayout {
    partitiontable: PartitionTable,
}

#[derive(Deserialize)]
struct PartitionTable {
    sectorsize: u64,
    partitions: Vec<Partition>,
}

#[derive(Deserialize)]
struct Partition {
    start: u64,
    size: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutSpec {
    schema: u32,
    partitions: Vec<PartitionSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionSpec {
    size_mib: Option<u64>,
    label: String,
    source: Option<PathBuf>,
    #[serde(skip)]
    entries: Vec<SourceEntry>,
}

struct SourceEntry {
    source: PathBuf,
    path: String,
    directory: bool,
}

fn partitions(layout: Option<&Path>, label: &str) -> Result<Vec<PartitionSpec>> {
    let (mut specs, base) = if let Some(path) = layout {
        let path = path.canonicalize().context("failed to resolve --layout")?;
        let spec: LayoutSpec = toml::from_str(&fs::read_to_string(&path)?)
            .with_context(|| format!("invalid disk layout {}", path.display()))?;
        if spec.schema != 1 {
            bail!("unsupported disk layout schema {}", spec.schema);
        }
        (spec.partitions, path.parent().unwrap().to_path_buf())
    } else {
        (
            vec![PartitionSpec {
                size_mib: None,
                label: label.to_owned(),
                source: None,
                entries: Vec::new(),
            }],
            PathBuf::new(),
        )
    };
    if !(1..=4).contains(&specs.len()) {
        bail!("disk layout requires 1..=4 primary FAT32 partitions");
    }
    let last = specs.len() - 1;
    for (index, spec) in specs.iter_mut().enumerate() {
        if spec.size_mib.is_none() && index != last {
            bail!("only the last partition may omit size_mib");
        }
        if spec.size_mib.is_some_and(|size| size < 64) {
            bail!("partition size_mib must be at least 64");
        }
        if spec.label.is_empty()
            || spec.label.len() > 11
            || !spec
                .label
                .bytes()
                .all(|b| b == b' ' || b.is_ascii_graphic())
        {
            bail!("FAT labels must contain 1..=11 printable ASCII bytes");
        }
        if let Some(source) = &spec.source {
            // The explicitly selected canonical root is the source boundary;
            // absolute roots and relative ../ references are valid inputs.
            let root = base
                .join(source)
                .canonicalize()
                .with_context(|| format!("failed to resolve source {}", source.display()))?;
            collect_source(&root, &root, &mut spec.entries)?;
            spec.entries.sort_by(|a, b| a.path.cmp(&b.path));
        }
    }
    Ok(specs)
}

fn collect_source(root: &Path, directory: &Path, entries: &mut Vec<SourceEntry>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let source = entry.path();
        let metadata = fs::symlink_metadata(&source)?;
        if !metadata.is_file() && !metadata.is_dir() {
            bail!(
                "disk source rejects symlinks and special files: {}",
                source.display()
            );
        }
        source.to_str().context("disk source paths must be UTF-8")?;
        let name = entry.file_name();
        let name = name.to_str().context("disk source names must be UTF-8")?;
        if name.ends_with([' ', '.'])
            || name
                .chars()
                .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        {
            bail!("unsupported FAT filename {}", source.display());
        }
        let path = source.strip_prefix(root)?.to_str().unwrap().to_owned();
        entries.push(SourceEntry {
            source: source.clone(),
            path,
            directory: metadata.is_dir(),
        });
        if metadata.is_dir() {
            collect_source(root, &source, entries)?;
        }
    }
    Ok(())
}

fn image_bytes(size_mib: u64) -> Result<u64> {
    if size_mib < 65 {
        bail!("--size-mib must be at least 65 for a FAT32 data disk");
    }
    let bytes = size_mib.checked_mul(MIB).context("disk size overflow")?;
    u32::try_from(bytes / SECTOR_SIZE).context("disk exceeds the MBR 32-bit sector range")?;
    Ok(bytes)
}

/// Create a new MBR/FAT32 disk with optional per-partition source trees.
/// Host-only work; uses private staging and bounded tools. Inventories are
/// sorted and FAT timestamps normalized; no source metadata is changed.
/// Publication is an atomic no-clobber hard link after all partitions succeed.
/// # Arguments
/// * `output` - New image filename in an existing directory on a filesystem
///   supporting hard links; symlinks and other existing entries are rejected.
/// * `size_mib` - Total disk size; at least 65 MiB. sfdisk chooses alignment.
/// * `label` - Default single-partition label; ignored when `layout` is supplied.
/// * `layout` - Optional schema-1 TOML with 1..=4 partition declarations.
///   Relative source paths resolve against it; absolute roots are accepted.
///   Only the last size may be omitted.
/// # Returns
/// Success after syncing and publishing the image. With no layout, the disk has
/// one empty partition. Source trees are copied verbatim; no bootloader or
/// mount configuration is generated. Mount policy comes from initramfs fstab.
/// # Errors
/// Invalid size/label/path or TOML/sources, existing destination, filesystem
/// failures, missing host tools, partitioning/formatting/copy failure
/// or timeout. Symlinks, special files and conflicting FAT names are rejected.
/// Unpublished work is removed on normal errors; existing paths are unchanged.
pub(crate) fn create(
    output: &Path,
    size_mib: u64,
    label: &str,
    layout: Option<&Path>,
) -> Result<()> {
    let bytes = image_bytes(size_mib)?;
    let specs = partitions(layout, label)?;
    let name = output.file_name().context("--output requires a filename")?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .context("--output parent directory must exist")?;
    let output = parent.join(name);
    if output.to_str().is_none() {
        bail!("--output requires a UTF-8 path for QEMU attachment");
    }
    match fs::symlink_metadata(&output) {
        Ok(_) => bail!("refusing to overwrite existing path {}", output.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("failed to inspect output path"),
    }

    let staging = Staging::new(&parent)?;
    let image = staging.0.join("image.raw");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&image)?;
    file.set_len(bytes)?;
    let image_path = image.to_str().unwrap();
    // Leave starts unspecified; sfdisk owns layout and alignment.
    // A fixed disk ID complements mkfs.fat --invariant for reproducibility.
    let mut script = String::from("label: dos\nlabel-id: 0x00000000\n\n");
    for spec in &specs {
        if let Some(size) = spec.size_mib {
            script.push_str(&format!("size={size}MiB, type=c\n"));
        } else {
            script.push_str("type=c\n");
        }
    }
    run("sfdisk", &["--quiet", image_path], Some(script.as_bytes()))?;
    let table = run("sfdisk", &["--json", image_path], None)?;
    let layout: DiskLayout = serde_json::from_slice(&table.stdout)?;
    let table = layout.partitiontable;
    if table.sectorsize != SECTOR_SIZE || table.partitions.len() != specs.len() {
        bail!("unexpected partition count or sector size from sfdisk");
    }
    let mut previous_end = 1;
    for partition in &table.partitions {
        let end = partition
            .start
            .checked_add(partition.size)
            .context("partition extent overflow")?;
        if partition.start < previous_end
            || end > bytes / SECTOR_SIZE
            || partition.size < 64 * MIB / SECTOR_SIZE
        {
            bail!("FAT32 partitions must be disjoint, in bounds, and at least 64 MiB");
        }
        previous_end = end;
    }
    for (partition, spec) in table.partitions.iter().zip(&specs) {
        run(
            "mkfs.fat",
            &[
                "--invariant",
                "-F",
                "32",
                "-S",
                "512",
                "-h",
                &partition.start.to_string(),
                "-n",
                &spec.label,
                &format!("--offset={}", partition.start),
                image_path,
                &(partition.size * SECTOR_SIZE / 1024).to_string(),
            ],
            None,
        )?;
        let image_spec = format!("{image_path}@@{}", partition.start * SECTOR_SIZE);
        for entry in &spec.entries {
            let target = format!("::/{}", entry.path);
            if entry.directory {
                run("mmd", &["-D", "s", "-i", &image_spec, &target], None)?;
            } else {
                run(
                    "mcopy",
                    &[
                        "-D",
                        "s",
                        "-i",
                        &image_spec,
                        entry.source.to_str().unwrap(),
                        &target,
                    ],
                    None,
                )?;
            }
        }
    }
    file.sync_all()?;
    // Unlike rename, hard_link cannot replace an entry created during formatting.
    // Staging is on the same filesystem and is removed after publication.
    fs::hard_link(&image, &output).with_context(|| {
        format!(
            "cannot publish {}; no existing path was replaced",
            output.display()
        )
    })?;
    println!("created {} ({size_mib} MiB)", output.display());
    for (index, (partition, spec)) in table.partitions.iter().zip(&specs).enumerate() {
        println!(
            "partition {}: FAT32, start={}, sectors={}, label={}, entries={}",
            index + 1,
            partition.start,
            partition.size,
            spec.label,
            spec.entries.len()
        );
    }
    println!("Configure /etc/fstab in initramfs to mount this disk; see docs/storage.md.");
    Ok(())
}

fn run(tool: &str, arguments: &[&str], input: Option<&[u8]>) -> Result<Output> {
    let mut child = Command::new("timeout")
        .args(["--kill-after=2s", "60s", tool])
        .args(arguments)
        .env("TZ", "UTC")
        .env("SOURCE_DATE_EPOCH", "315532800") // FAT epoch: 1980-01-01.
        .env("LC_ALL", "C.UTF-8")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!("failed to run {tool}; see docs/storage.md for host dependencies")
        })?;
    let submitted = input.map(|bytes| {
        child
            .stdin
            .take()
            .expect("piped tool input")
            .write_all(bytes)
    });
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "{tool} failed ({}): {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if let Some(submitted) = submitted {
        submitted.context("failed to submit tool input")?;
    }
    Ok(output)
}

struct Staging(PathBuf);

impl Staging {
    fn new(parent: &Path) -> Result<Self> {
        for _ in 0..128 {
            let path = parent.join(format!(
                ".genrt-disk-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("failed to create staging directory"),
            }
        }
        bail!("could not reserve a private staging directory")
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests;
