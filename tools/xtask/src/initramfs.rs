use std::{
    collections::HashSet,
    fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MANIFEST_SCHEMA: u32 = 1;
const CPIO_TYPE_MASK: u32 = 0o170000;
const CPIO_DIRECTORY: u32 = 0o040000;
const CPIO_REGULAR: u32 = 0o100000;
const TEST_ARTIFACT_MARKER: &[u8] = b"GENRT_TEST_ARTIFACT_V1";
const TEST_ARTIFACT_SECTION: &[u8] = b".genrt.test_marker";
const TEST_PROTOCOL_MAGIC: &[u8] = b"GTRT/1|";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Manifest {
    schema: u32,
    entrypoint: String,
    entries: Vec<ManifestEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ManifestEntry {
    path: String,
    kind: EntryKind,
    origin: Origin,
    mode: u32,
    size: u64,
    sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EntryKind {
    Directory,
    File,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Build-time origin attached to every manifested archive entry.
pub(crate) enum Origin {
    /// Product root data or an executable declared by the product manifest.
    Production,
    /// Stable data or helper executable owned by a contract test.
    TestFixture,
    /// Test-only `/init` process that owns terminal protocol status.
    TestSupervisor,
}

#[derive(Clone, Debug, Default)]
/// Prefix map used to assign entry origins while collecting a staging tree.
pub(crate) struct Provenance {
    prefixes: Vec<(String, Origin)>,
}

impl Provenance {
    /// Mark one canonical path and all descendants with an origin.
    ///
    /// # Arguments
    ///
    /// * `path` - Canonical archive-relative path prefix.
    /// * `origin` - Origin assigned to matching entries.
    ///
    /// # Returns
    ///
    /// Returns success after recording the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error when `path` is not canonical.
    pub(crate) fn mark_prefix(&mut self, path: &str, origin: Origin) -> Result<()> {
        validate_archive_path(path)?;
        self.prefixes.push((path.to_owned(), origin));
        Ok(())
    }

    fn origin_for(&self, path: &str) -> Origin {
        self.prefixes
            .iter()
            .filter(|(prefix, _)| path == prefix || path.starts_with(&format!("{prefix}/")))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, origin)| *origin)
            .unwrap_or(Origin::Production)
    }
}

struct SourceEntry {
    manifest: ManifestEntry,
    source: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Content policy applied while reopening a generated initramfs.
pub(crate) enum Policy {
    /// Permit test-only paths and protocol strings.
    Test,
    /// Reject test-only paths and protocol strings.
    Production,
}

/// Derive the JSON sidecar path for an initramfs archive.
///
/// # Arguments
///
/// * `archive` - CPIO archive path.
///
/// # Returns
///
/// Returns the same path with its extension replaced by `manifest.json`.
pub(crate) fn manifest_path(archive: &Path) -> PathBuf {
    archive.with_extension("manifest.json")
}

/// Build, reproduce, manifest, and verify one deterministic newc archive.
///
/// # Arguments
///
/// * `root` - Staging directory whose descendants become archive entries.
/// * `archive` - Destination CPIO path.
/// * `policy` - Selects test or production content validation.
/// * `provenance` - Origin prefixes recorded in the generated manifest.
///
/// # Returns
///
/// Returns the generated JSON manifest path.
///
/// # Errors
///
/// Returns an error for invalid paths or file types, I/O and serialization
/// failures, nondeterministic output, or structural/policy violations.
pub(crate) fn build(
    root: &Path,
    archive: &Path,
    policy: Policy,
    provenance: &Provenance,
) -> Result<PathBuf> {
    let entries = collect_entries(root, provenance)?;
    ensure_entrypoint(&entries)?;
    if let Some(parent) = archive.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    write_archive(&entries, archive)?;

    let determinism = archive.with_extension("determinism.cpio");
    write_archive(&entries, &determinism)?;
    let archive_bytes = fs::read(archive)?;
    let repeated_bytes = fs::read(&determinism)?;
    fs::remove_file(&determinism)?;
    if archive_bytes != repeated_bytes {
        bail!("initramfs serialization is not deterministic");
    }

    let manifest = Manifest {
        schema: MANIFEST_SCHEMA,
        entrypoint: "init".to_owned(),
        entries: entries.iter().map(|entry| entry.manifest.clone()).collect(),
    };
    let manifest_path = manifest_path(archive);
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    verify(archive, &manifest_path, policy)?;
    Ok(manifest_path)
}

/// Reopen an archive and compare it with its generated manifest.
///
/// # Arguments
///
/// * `archive` - Existing newc archive.
/// * `manifest_path` - Expected JSON manifest.
/// * `policy` - Selects test or production content validation.
///
/// # Returns
///
/// Returns success only when every archive entry matches the manifest.
///
/// # Errors
///
/// Returns an error for malformed inputs, duplicate/noncanonical paths,
/// hashes or metadata mismatches, invalid executable ELF files, and policy
/// violations.
pub(crate) fn verify(archive: &Path, manifest_path: &Path, policy: Policy) -> Result<()> {
    let expected: Manifest = serde_json::from_slice(
        &fs::read(manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?,
    )?;
    if expected.schema != MANIFEST_SCHEMA || expected.entrypoint != "init" {
        bail!("unsupported initramfs manifest metadata");
    }
    let actual = read_archive_manifest(archive, policy, &expected)?;
    if actual != expected {
        bail!(
            "initramfs {} does not match {}",
            archive.display(),
            manifest_path.display()
        );
    }
    Ok(())
}

/// Return the recorded SHA-256 for one file in an initramfs manifest.
///
/// # Arguments
///
/// * `manifest_path` - JSON sidecar produced with the archive.
/// * `path` - Canonical archive-relative file path.
///
/// # Returns
///
/// Returns the file hash when the path names a regular entry, or `None` when
/// no such regular entry exists.
///
/// # Errors
///
/// Returns an error when the manifest cannot be read or parsed.
pub(crate) fn entry_sha256(manifest_path: &Path, path: &str) -> Result<Option<String>> {
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    Ok(manifest
        .entries
        .into_iter()
        .find(|entry| entry.path == path && entry.kind == EntryKind::File)
        .map(|entry| entry.sha256))
}

fn collect_entries(root: &Path, provenance: &Provenance) -> Result<Vec<SourceEntry>> {
    let mut entries = Vec::new();
    collect_directory(root, root, provenance, &mut entries)?;
    entries.sort_by(|lhs, rhs| lhs.manifest.path.cmp(&rhs.manifest.path));
    let mut paths = HashSet::new();
    for entry in &entries {
        if !paths.insert(entry.manifest.path.clone()) {
            bail!("duplicate initramfs path {}", entry.manifest.path);
        }
    }
    Ok(entries)
}

fn collect_directory(
    root: &Path,
    directory: &Path,
    provenance: &Provenance,
    out: &mut Vec<SourceEntry>,
) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
    {
        let entry = entry?;
        let source = entry.path();
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            bail!("initramfs rejects symlink {}", source.display());
        }
        let path = canonical_archive_path(root, &source)?;
        let (kind, data, mode) = if metadata.is_dir() {
            (EntryKind::Directory, Vec::new(), CPIO_DIRECTORY | 0o755)
        } else if metadata.is_file() {
            let data = fs::read(&source)?;
            let permissions = if is_aarch64_exec(&data) { 0o755 } else { 0o644 };
            (EntryKind::File, data, CPIO_REGULAR | permissions)
        } else {
            bail!("initramfs rejects unsupported file {}", source.display());
        };
        out.push(SourceEntry {
            manifest: ManifestEntry {
                origin: provenance.origin_for(&path),
                path,
                kind,
                mode,
                size: data.len() as u64,
                sha256: sha256_bytes(&data),
            },
            source: source.clone(),
        });
        if metadata.is_dir() {
            collect_directory(root, &source, provenance, out)?;
        }
    }
    Ok(())
}

fn write_archive(entries: &[SourceEntry], output: &Path) -> Result<()> {
    let mut cpio_entries = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let data = match entry.manifest.kind {
            EntryKind::Directory => Vec::new(),
            EntryKind::File => fs::read(&entry.source)?,
        };
        let file_type = match entry.manifest.kind {
            EntryKind::Directory => cpio::newc::ModeFileType::Directory,
            EntryKind::File => cpio::newc::ModeFileType::Regular,
        };
        let builder = cpio::NewcBuilder::new(&entry.manifest.path)
            .ino(u32::try_from(index).context("too many initramfs entries")?)
            .uid(0)
            .gid(0)
            .mode(entry.manifest.mode & !CPIO_TYPE_MASK)
            .set_mode_file_type(file_type)
            .mtime(0)
            .nlink(1);
        cpio_entries.push((builder, Cursor::new(data)));
    }
    let file = fs::File::create(output)?;
    cpio::write_cpio(cpio_entries.into_iter(), file)?;
    Ok(())
}

fn read_archive_manifest(archive: &Path, policy: Policy, expected: &Manifest) -> Result<Manifest> {
    let bytes = fs::read(archive)?;
    let mut cursor = Cursor::new(bytes.as_slice());
    let mut entries = Vec::new();
    let mut paths = HashSet::new();
    loop {
        let mut reader = cpio::NewcReader::new(cursor).context("invalid newc archive")?;
        if reader.entry().is_trailer() {
            cursor = reader.finish()?;
            let trailing = &cursor.get_ref()[cursor.position() as usize..];
            if trailing.iter().any(|byte| *byte != 0) {
                bail!("non-zero data follows the CPIO trailer");
            }
            break;
        }
        let path = reader.entry().name().to_owned();
        validate_archive_path(&path)?;
        if !paths.insert(path.clone()) {
            bail!("duplicate CPIO entry {path}");
        }
        let origin = expected
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.origin)
            .unwrap_or(Origin::Production);
        if policy == Policy::Production
            && (path == ".__genrt_test__" || path.starts_with(".__genrt_test__/"))
        {
            bail!("production initramfs uses reserved test namespace {path}");
        }
        if policy == Policy::Production && origin != Origin::Production {
            bail!("production initramfs contains non-production entry {path}");
        }
        let mode = reader.entry().mode();
        let kind = match mode & CPIO_TYPE_MASK {
            CPIO_DIRECTORY => EntryKind::Directory,
            CPIO_REGULAR => EntryKind::File,
            other => bail!("unsupported CPIO file type {other:o} for {path}"),
        };
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;
        cursor = reader.finish()?;
        if kind == EntryKind::Directory && !data.is_empty() {
            bail!("directory {path} has file data");
        }
        if kind == EntryKind::File && mode & 0o111 != 0 {
            validate_aarch64_exec(&data)
                .with_context(|| format!("invalid executable initramfs entry {path}"))?;
            if policy == Policy::Production
                && (find_subslice(&data, TEST_ARTIFACT_MARKER)
                    || find_subslice(&data, TEST_ARTIFACT_SECTION))
            {
                bail!("production executable {path} contains the test artifact marker");
            }
            if policy == Policy::Production && find_subslice(&data, TEST_PROTOCOL_MAGIC) {
                bail!("production executable {path} contains test protocol code");
            }
        }
        entries.push(ManifestEntry {
            path,
            kind,
            origin,
            mode,
            size: data.len() as u64,
            sha256: sha256_bytes(&data),
        });
    }
    entries.sort_by(|lhs, rhs| lhs.path.cmp(&rhs.path));
    if !entries
        .iter()
        .any(|entry| entry.path == "init" && entry.kind == EntryKind::File)
    {
        bail!("initramfs archive has no regular init entry");
    }
    Ok(Manifest {
        schema: MANIFEST_SCHEMA,
        entrypoint: "init".to_owned(),
        entries,
    })
}

fn canonical_archive_path(root: &Path, source: &Path) -> Result<String> {
    let relative = source.strip_prefix(root)?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            bail!("non-canonical initramfs path {}", source.display());
        };
        let part = part
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 initramfs path"))?;
        if part.is_empty() || part == "." || part == ".." || part.contains('/') {
            bail!("invalid initramfs path component {part:?}");
        }
        parts.push(part);
    }
    if parts.is_empty() {
        bail!("empty initramfs archive path");
    }
    Ok(parts.join("/"))
}

fn validate_archive_path(path: &str) -> Result<()> {
    if path.is_empty() || path.starts_with('/') || path.ends_with('/') {
        bail!("non-canonical CPIO path {path:?}");
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            bail!("non-canonical CPIO path {path:?}");
        }
    }
    Ok(())
}

fn ensure_entrypoint(entries: &[SourceEntry]) -> Result<()> {
    let matching = entries
        .iter()
        .filter(|entry| entry.manifest.path == "init" && entry.manifest.kind == EntryKind::File)
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        bail!("initramfs must contain exactly one regular init entry");
    }
    if matching[0].manifest.mode & 0o111 == 0 {
        bail!("initramfs init entry is not an AArch64 executable");
    }
    Ok(())
}

fn validate_aarch64_exec(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 64 || &bytes[..4] != b"\x7fELF" {
        bail!("not an ELF image");
    }
    if bytes[4] != 2 || bytes[5] != 1 {
        bail!("ELF is not 64-bit little-endian");
    }
    if u16::from_le_bytes([bytes[16], bytes[17]]) != 2 {
        bail!("ELF is not ET_EXEC");
    }
    if u16::from_le_bytes([bytes[18], bytes[19]]) != 183 {
        bail!("ELF is not AArch64");
    }
    Ok(())
}

fn is_aarch64_exec(bytes: &[u8]) -> bool {
    validate_aarch64_exec(bytes).is_ok()
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("genrt-xtask-{name}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn minimal_aarch64_elf() -> Vec<u8> {
        let mut elf = vec![0u8; 64];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[18..20].copy_from_slice(&183u16.to_le_bytes());
        elf
    }

    #[test]
    fn rejects_noncanonical_archive_paths() {
        for path in ["", "/init", "a/../b", "a//b", "a/./b", "a/"] {
            assert!(validate_archive_path(path).is_err(), "accepted {path:?}");
        }
        assert!(validate_archive_path("bin/echo").is_ok());
    }

    #[test]
    fn validates_aarch64_exec_identity() {
        let mut elf = minimal_aarch64_elf();
        assert!(validate_aarch64_exec(&elf).is_ok());
        elf[18] = 62;
        assert!(validate_aarch64_exec(&elf).is_err());
    }

    #[test]
    fn detects_manifest_hash_mismatch_and_invalid_init() {
        let root = temp_dir("manifest-mismatch");
        fs::write(root.join("init"), minimal_aarch64_elf()).unwrap();
        let archive = root.join("image.cpio");
        let manifest_path = build(&root, &archive, Policy::Test, &Provenance::default()).unwrap();
        let mut manifest: Manifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest.entries[0].sha256 = "00".to_owned();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(verify(&archive, &manifest_path, Policy::Test).is_err());

        let invalid = temp_dir("invalid-init");
        fs::write(invalid.join("init"), b"not an elf").unwrap();
        assert!(
            build(
                &invalid,
                &invalid.join("image.cpio"),
                Policy::Test,
                &Provenance::default()
            )
            .is_err()
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(invalid);
    }

    #[test]
    fn rejects_duplicate_archive_entries() {
        let root = temp_dir("duplicate-entry");
        let archive = root.join("duplicate.cpio");
        let elf = minimal_aarch64_elf();
        let entries = [0u32, 1u32].into_iter().map(|ino| {
            (
                cpio::NewcBuilder::new("init")
                    .ino(ino)
                    .mode(0o755)
                    .set_mode_file_type(cpio::newc::ModeFileType::Regular),
                Cursor::new(elf.clone()),
            )
        });
        cpio::write_cpio(entries, fs::File::create(&archive).unwrap()).unwrap();
        let manifest = Manifest {
            schema: MANIFEST_SCHEMA,
            entrypoint: "init".to_owned(),
            entries: Vec::new(),
        };
        let manifest_path = manifest_path(&archive);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(verify(&archive, &manifest_path, Policy::Test).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn production_policy_uses_provenance_and_exact_executable_magic() {
        let root = temp_dir("production-policy");
        let source = root.join("root");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("init"), minimal_aarch64_elf()).unwrap();
        fs::write(
            source.join("notes"),
            b"GTRT/1 is mentioned in documentation",
        )
        .unwrap();
        let archive = root.join("production.cpio");
        let manifest = build(&source, &archive, Policy::Test, &Provenance::default()).unwrap();
        verify(&archive, &manifest, Policy::Production).unwrap();

        let mut provenance = Provenance::default();
        provenance
            .mark_prefix("init", Origin::TestSupervisor)
            .unwrap();
        let test_archive = root.join("test.cpio");
        let test_manifest = build(&source, &test_archive, Policy::Test, &provenance).unwrap();
        assert!(verify(&test_archive, &test_manifest, Policy::Production).is_err());

        let mut marked_elf = minimal_aarch64_elf();
        marked_elf.extend_from_slice(TEST_ARTIFACT_MARKER);
        fs::write(source.join("init"), marked_elf).unwrap();
        let marked_archive = root.join("marked.cpio");
        let marked_manifest = build(
            &source,
            &marked_archive,
            Policy::Test,
            &Provenance::default(),
        )
        .unwrap();
        assert!(verify(&marked_archive, &marked_manifest, Policy::Production).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
