//! Explicitly owned storage-contract fixture. Never formats a user --disk path.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
};

const OFFSET: u64 = 1024 * 1024;
const PARTITION_BYTES: u64 = 64 * 1024 * 1024;
const PAYLOAD_BYTES: usize = 65_573;

/// Create a fresh explicitly owned MBR/FAT32 fixture, never a user disk.
/// # Arguments
/// * `directory` - Case artifact directory owning all generated files.
/// # Returns
/// Canonical raw-image path, after bounded formatting and host seeding.
/// # Errors
/// Filesystem, invalid path, missing host tools or bounded subprocess failures.
pub(super) fn prepare(directory: &Path) -> Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let image = directory.join("storage-test.raw");
    if image.exists() || image.is_symlink() {
        fs::remove_file(&image)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&image)?;
    file.set_len(OFFSET + 2 * PARTITION_BYTES)?;
    let mut mbr = [0u8; 512];
    for index in 0..2 {
        let entry = 446 + index * 16;
        let start = OFFSET + index as u64 * PARTITION_BYTES;
        mbr[entry + 4] = 0x0c;
        mbr[entry + 8..entry + 12].copy_from_slice(&((start / 512) as u32).to_le_bytes());
        mbr[entry + 12..entry + 16]
            .copy_from_slice(&((PARTITION_BYTES / 512) as u32).to_le_bytes());
    }
    mbr[510..].copy_from_slice(&[0x55, 0xaa]);
    file.write_all(&mbr)?;
    file.sync_all()?;
    drop(file);
    for (index, label) in ["GENRTTEST", "GENRTEXTRA"].into_iter().enumerate() {
        let sector = ((OFFSET + index as u64 * PARTITION_BYTES) / 512).to_string();
        run(
            "mkfs.fat",
            &[
                "--invariant",
                "-F",
                "32",
                "-s",
                "1",
                "-i",
                &format!("0024002{}", index + 4),
                "-n",
                label,
                "-h",
                &sector,
                &format!("--offset={sector}"),
                image.to_str().context("non-UTF8 test image")?,
                &(PARTITION_BYTES / 1024).to_string(),
            ],
            None,
        )?;
    }
    let spec = format!("{}@@{OFFSET}", image.canonicalize()?.display());
    run("mmd", &["-i", &spec, "::/SUB"], None)?;
    run("mmd", &["-i", &spec, "::/EXTRA"], None)?;
    let seed = directory.join("seed.txt");
    fs::write(&seed, b"host FAT seed\n")?;
    run(
        "mcopy",
        &[
            "-i",
            &spec,
            seed.to_str().context("non-UTF8 test path")?,
            "::/SUB/seed.txt",
        ],
        None,
    )?;
    let extra = directory.join("extra.txt");
    fs::write(&extra, b"second FAT seed\n")?;
    let extra_spec = format!(
        "{}@@{}",
        image.canonicalize()?.display(),
        OFFSET + PARTITION_BYTES
    );
    run(
        "mcopy",
        &[
            "-i",
            &extra_spec,
            extra.to_str().context("non-UTF8 test path")?,
            "::/seed.txt",
        ],
        None,
    )?;
    let mut path = "::/DEEP".to_owned();
    let mut directories = vec!["-i".to_owned(), spec.clone()];
    for depth in 0..31 {
        directories.push(path.clone());
        if depth != 30 {
            path.push_str("/d");
        }
    }
    run(
        "mmd",
        &directories.iter().map(String::as_str).collect::<Vec<_>>(),
        None,
    )?;
    run(
        "mcopy",
        &[
            "-i",
            &spec,
            seed.to_str().context("non-UTF8 seed")?,
            &format!("{path}/leaf.txt"),
        ],
        None,
    )?;
    crate::qemu::validate_disk(&image)
}

/// Independently extract, compare and structurally check the stopped guest disk.
/// # Arguments
/// * `directory` - Same owned case directory used by `prepare`, after QEMU reap.
/// # Returns
/// Success after exact payload comparison and fsck; retains checksum/evidence.
/// # Errors
/// Missing/corrupt payload, mismatch, host-tool timeout/failure or filesystem I/O.
pub(super) fn verify(directory: &Path) -> Result<()> {
    let image = directory.join("storage-test.raw");
    let spec = format!("{}@@{OFFSET}", image.canonicalize()?.display());
    for (name, expected) in [
        ("REDIRECT.TXT", b"first\nsecond\n".as_slice()),
        ("COPY.TXT", b"first\nsecond\nrecovered\n".as_slice()),
    ] {
        let extracted = directory.join(format!("{name}.host.txt"));
        run(
            "mcopy",
            &["-o", "-i", &spec, &format!("::/{name}")],
            Some(&extracted),
        )?;
        if fs::read(&extracted)? != expected {
            bail!("host extraction differs from production shell output in {name}");
        }
    }
    let output = directory.join("persist.host.bin");
    run(
        "mcopy",
        &["-o", "-i", &spec, "::/PERSIST.BIN"],
        Some(&output),
    )?;
    let actual = fs::read(&output)?;
    let expected = (0..PAYLOAD_BYTES)
        .map(|i| (i.wrapping_mul(37).wrapping_add(11)) as u8)
        .collect::<Vec<_>>();
    if actual != expected {
        bail!("host FAT extraction differs from expected persistent payload");
    }
    let hash = format!("{:x}", Sha256::digest(&actual));
    fs::write(
        directory.join("persistence.sha256"),
        format!("{hash}  PERSIST.BIN\n"),
    )?;
    // fsck needs a volume, not an MBR image. Extract a bounded test-only copy.
    for index in 0..2 {
        let mut source = fs::File::open(&image)?;
        source.seek(SeekFrom::Start(OFFSET + index * PARTITION_BYTES))?;
        let partition = directory.join(format!("partition{}.host.fat", index + 1));
        let mut destination = fs::File::create(&partition)?;
        let copied = std::io::copy(
            &mut std::io::Read::take(source, PARTITION_BYTES),
            &mut destination,
        )?;
        if copied != PARTITION_BYTES {
            bail!("short FAT partition extraction");
        }
        drop(destination);
        run("fsck.fat", &["-n"], Some(&partition))?;
    }
    Ok(())
}

fn run(tool: &str, arguments: &[&str], last: Option<&Path>) -> Result<()> {
    let mut command = Command::new("timeout");
    command.args(["20s", tool]).args(arguments);
    if let Some(path) = last {
        command.arg(path);
    }
    let output = command
        .output()
        .with_context(|| format!("failed to run {tool}; install dosfstools and mtools"))?;
    if !output.status.success() {
        bail!(
            "{tool} failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistence_fixture_has_bounded_sector_aligned_partitions() {
        assert_eq!(OFFSET % 512, 0);
        assert_eq!(PARTITION_BYTES % 512, 0);
        assert!(PAYLOAD_BYTES > 4096 && PAYLOAD_BYTES < PARTITION_BYTES as usize);
    }
}
