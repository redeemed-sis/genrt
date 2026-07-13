use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use flate2::{Compression, GzBuilder};
use semver::Version;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tar::{Builder, Header};

use crate::{
    artifacts::{AARCH64_TARGET, Profile},
    cli::LogLevel,
    initramfs, qemu,
    test::{self, Options, PreparedArtifacts},
    workflow,
};

#[derive(Serialize)]
struct Manifest {
    project: &'static str,
    tag: String,
    prerelease: bool,
    git_commit: String,
    target: &'static str,
    cargo_profile: &'static str,
    kernel_features: Vec<String>,
    rustc_version: String,
    cargo_version: String,
    qemu_version: String,
    clang_version: String,
    lld_version: String,
    files: Vec<ManifestFile>,
}

#[derive(Serialize)]
struct ManifestFile {
    path: String,
    sha256: String,
}

/// Verify and package a tagged AArch64 QEMU release.
///
/// # Arguments
///
/// * tag - Version label in v-prefixed SemVer form.
/// * output_dir - Directory that receives the archive and SHA256SUMS.
///
/// # Returns
///
/// Returns success after checks, contract suites, production artifact
/// verification, and deterministic packaging complete.
///
/// # Errors
///
/// Returns an error for an invalid/unreachable tag, any failed gate, tool or
/// filesystem failure, or checksum/package generation failure.
pub(crate) fn run(tag: &str, output_dir: &Path) -> Result<()> {
    let version = parse_release_tag(tag)?;
    verify_release_source(tag)?;
    verify_existing_tag_reachable_from_main(tag)?;

    workflow::check()?;
    test::run(Options::ci_default())?;

    let artifacts = workflow::build_aarch64(Profile::Release, Some(LogLevel::Info), &[])?;
    crate::artifacts::verify_production_kernel(&artifacts.kernel_elf())?;
    let users = workflow::build_production_user_artifacts(Profile::Release, true)?;
    test::verify_product_contract_coverage(&users)?;

    let userspace_initramfs = test::prepare_userspace_contract_initramfs(
        Profile::Release,
        artifacts.root().join("release-userspace-contract.cpio"),
        &users,
    )?;
    run_prepared_contract("userspace-contract", &artifacts, userspace_initramfs)?;
    let userspace_contract_manifest =
        initramfs::manifest_path(&artifacts.root().join("release-userspace-contract.cpio"));
    let shell_initramfs = test::prepare_shell_contract_initramfs(
        Profile::Release,
        artifacts.root().join("release-shell-contract.cpio"),
        &users,
    )?;
    run_prepared_contract("shell-contract", &artifacts, shell_initramfs)?;

    let shell_contract_manifest =
        initramfs::manifest_path(&artifacts.root().join("release-shell-contract.cpio"));

    let release_initramfs =
        workflow::build_initramfs_with_users(Profile::Release, None, None, None, &users)?;
    let release_manifest = initramfs::manifest_path(&release_initramfs);
    initramfs::verify(
        &release_initramfs,
        &release_manifest,
        initramfs::Policy::Production,
    )?;
    verify_userspace_identity(
        &users,
        &release_manifest,
        &userspace_contract_manifest,
        &shell_contract_manifest,
    )?;
    let prepared = PreparedArtifacts {
        artifacts: artifacts.clone(),
        initramfs: release_initramfs,
    };

    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    package(tag, &version, output_dir, &prepared)
}

fn verify_userspace_identity(
    users: &workflow::ProductionUserArtifacts,
    release: &Path,
    userspace_contract: &Path,
    shell_contract: &Path,
) -> Result<()> {
    let invocation_plan = crate::product_contract::Plan::load()?;
    for program in users.programs() {
        let release_hash = initramfs::entry_sha256(release, program.install())?
            .ok_or_else(|| anyhow::anyhow!("release manifest has no {}", program.install()))?;
        let built_hash = sha256_file(program.elf())?;
        if release_hash != built_hash {
            bail!(
                "release {} differs from built {} ELF",
                program.install(),
                program.name()
            );
        }
        if program.contract() == "structural" {
            println!(
                "release coverage: {} present_in_contract_image=false invocation_declared=false invocation_executed=false contract_passed=false structural_only=true path={}",
                program.name(),
                program.install()
            );
            continue;
        }
        let contract_manifest = match program.contract() {
            "shell" => shell_contract,
            "userspace" => userspace_contract,
            other => bail!("unsupported product contract role {other:?}"),
        };
        let tested_hash = initramfs::entry_sha256(contract_manifest, program.contract_install())?
            .ok_or_else(|| {
            anyhow::anyhow!(
                "{} contract manifest has no {}",
                program.contract(),
                program.contract_install()
            )
        })?;
        if release_hash != tested_hash {
            bail!(
                "release {} differs from dynamically tested {} ELF",
                program.install(),
                program.name()
            );
        }
        let invocation = invocation_plan.for_program(program.name()).ok_or_else(|| {
            anyhow::anyhow!("missing invocation for dynamic program {}", program.name())
        })?;
        println!(
            "release coverage: {} present_in_contract_image=true invocation_declared=true invocation_executed=true contract_passed=true structural_only=false role={} case={} path={} expected_exit={}",
            program.name(),
            program.contract(),
            invocation.case(),
            invocation.path(),
            invocation.expected_exit()
        );
    }
    Ok(())
}

fn run_prepared_contract(
    case: &str,
    artifacts: &crate::artifacts::Aarch64Artifacts,
    initramfs: PathBuf,
) -> Result<()> {
    test::run(Options {
        case: Some(case.to_owned()),
        list: false,
        profile: Some(Profile::Release),
        timeout_secs: 90,
        artifacts_dir: PathBuf::from("target/production-contracts"),
        keep_going: false,
        prepared: Some(PreparedArtifacts {
            artifacts: artifacts.clone(),
            initramfs,
        }),
    })
}

fn parse_release_tag(tag: &str) -> Result<Version> {
    let raw = tag
        .strip_prefix('v')
        .ok_or_else(|| anyhow::anyhow!("release tag must start with 'v'"))?;
    if raw.is_empty() || raw.starts_with('v') {
        bail!("invalid release tag {tag:?}");
    }
    Version::parse(raw).with_context(|| format!("invalid SemVer release tag {tag:?}"))
}

fn verify_existing_tag_reachable_from_main(tag: &str) -> Result<()> {
    let tag_ref = format!("refs/tags/{tag}");
    if !command_success(Command::new("git").args(["show-ref", "--verify", "--quiet", &tag_ref]))? {
        println!("dist: {tag} is a local version label; no Git tag reachability check required");
        return Ok(());
    }
    let main_ref = if command_success(Command::new("git").args([
        "show-ref",
        "--verify",
        "--quiet",
        "refs/remotes/origin/main",
    ]))? {
        "refs/remotes/origin/main"
    } else {
        "refs/heads/main"
    };
    if !command_success(Command::new("git").args([
        "merge-base",
        "--is-ancestor",
        &tag_ref,
        main_ref,
    ]))? {
        bail!("tag {tag} is not reachable from {main_ref}");
    }
    Ok(())
}

fn verify_release_source(tag: &str) -> Result<()> {
    let tag_ref = format!("refs/tags/{tag}");
    if !command_success(Command::new("git").args(["show-ref", "--verify", "--quiet", &tag_ref]))? {
        return Ok(());
    }
    let tag_commit =
        command_line(Command::new("git").args(["rev-parse", &format!("{tag}^{{commit}}")]))?;
    let head = command_line(Command::new("git").args(["rev-parse", "HEAD"]))?;
    if tag_commit != head {
        bail!("release tag {tag} points to {tag_commit}, but HEAD is {head}");
    }
    let status = command_line(Command::new("git").args(["status", "--porcelain"]))?;
    if !status.is_empty() {
        bail!("release tag builds require a clean worktree");
    }
    Ok(())
}

fn package(
    tag: &str,
    version: &Version,
    output_dir: &Path,
    prepared: &PreparedArtifacts,
) -> Result<()> {
    let bundle_name = format!("genrt-aarch64-qemu-virt-{tag}");
    let staging = output_dir.join(".staging").join(&bundle_name);
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    let initramfs_manifest = initramfs::manifest_path(&prepared.initramfs);
    let files = [
        ("genrt-aarch64.elf", prepared.artifacts.kernel_elf()),
        ("qemu-virt.dtb", prepared.artifacts.dtb()),
        ("initramfs.cpio", prepared.initramfs.clone()),
        ("initramfs.manifest.json", initramfs_manifest),
    ];
    for (name, source) in &files {
        fs::copy(source, staging.join(name))
            .with_context(|| format!("failed to stage release artifact {}", source.display()))?;
    }

    let run_config = qemu::Config {
        kernel: PathBuf::from("genrt-aarch64.elf"),
        dtb: PathBuf::from("qemu-virt.dtb"),
        initramfs: PathBuf::from("initramfs.cpio"),
        wait_for_gdb: false,
    };
    fs::write(
        staging.join("RUN.md"),
        format!(
            "# Run genrt on QEMU virt\n\n```bash\n{}\n```\n",
            run_config.display()
        ),
    )?;

    let manifest = Manifest {
        project: "genrt",
        tag: tag.to_owned(),
        prerelease: !version.pre.is_empty(),
        git_commit: command_line(Command::new("git").args(["rev-parse", "HEAD"]))?,
        target: AARCH64_TARGET,
        cargo_profile: "release",
        kernel_features: vec!["log-level-info".to_owned()],
        rustc_version: command_line(Command::new("rustc").arg("--version"))?,
        cargo_version: command_line(Command::new("cargo").arg("--version"))?,
        qemu_version: command_first_line(Command::new("qemu-system-aarch64").arg("--version"))?,
        clang_version: command_first_line(Command::new("clang").arg("--version"))?,
        lld_version: command_first_line(Command::new("ld.lld").arg("--version"))?,
        files: files
            .iter()
            .map(|(name, _)| {
                Ok(ManifestFile {
                    path: (*name).to_owned(),
                    sha256: sha256_file(&staging.join(name))?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    fs::write(
        staging.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    let epoch = command_line(Command::new("git").args(["show", "-s", "--format=%ct", "HEAD"]))?
        .parse::<u64>()
        .context("tag commit timestamp is not an integer")?;
    let archive = output_dir.join(format!("{bundle_name}.tar.gz"));
    write_deterministic_archive(&archive, &bundle_name, &staging, epoch)?;
    let archive_name = archive.file_name().unwrap().to_string_lossy();
    let checksum = format!("{}  {archive_name}\n", sha256_file(&archive)?);
    fs::write(output_dir.join("SHA256SUMS"), checksum)?;
    fs::remove_dir_all(output_dir.join(".staging"))?;
    println!("built {}", archive.display());
    Ok(())
}

fn write_deterministic_archive(
    output: &Path,
    root_name: &str,
    root: &Path,
    epoch: u64,
) -> Result<()> {
    let file = File::create(output)?;
    let encoder = GzBuilder::new()
        .mtime(u32::try_from(epoch).unwrap_or(u32::MAX))
        .write(file, Compression::best());
    let mut tar = Builder::new(encoder);
    for name in [
        "genrt-aarch64.elf",
        "qemu-virt.dtb",
        "initramfs.cpio",
        "initramfs.manifest.json",
        "manifest.json",
        "RUN.md",
    ] {
        let path = root.join(name);
        let mut source = File::open(&path)?;
        let size = source.metadata()?.len();
        let mut header = Header::new_gnu();
        header.set_size(size);
        header.set_mode(if name.ends_with(".elf") { 0o755 } else { 0o644 });
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(epoch);
        header.set_cksum();
        tar.append_data(&mut header, format!("{root_name}/{name}"), &mut source)?;
    }
    tar.into_inner()?.finish()?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn command_success(command: &mut Command) -> Result<bool> {
    Ok(command.status()?.success())
}

fn command_line(command: &mut Command) -> Result<String> {
    let output = command.output()?;
    if !output.status.success() {
        bail!(
            "command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn command_first_line(command: &mut Command) -> Result<String> {
    Ok(command_line(command)?
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_release_tags() {
        assert!(parse_release_tag("v0.1.0").is_ok());
        assert!(parse_release_tag("v1.0.0-rc.2").is_ok());
        assert!(parse_release_tag("1.0.0").is_err());
        assert!(parse_release_tag("vnot-semver").is_err());
    }

    #[test]
    fn manifest_and_checksum_are_stable_json_and_text() {
        let manifest = Manifest {
            project: "genrt",
            tag: "v1.0.0".to_owned(),
            prerelease: false,
            git_commit: "abc".to_owned(),
            target: AARCH64_TARGET,
            cargo_profile: "release",
            kernel_features: Vec::new(),
            rustc_version: "rustc".to_owned(),
            cargo_version: "cargo".to_owned(),
            qemu_version: "qemu".to_owned(),
            clang_version: "clang".to_owned(),
            lld_version: "lld".to_owned(),
            files: vec![ManifestFile {
                path: "kernel".to_owned(),
                sha256: "00".to_owned(),
            }],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("\"tag\":\"v1.0.0\""));
        assert_eq!(
            format!("{}  {}\n", "00", "archive.tar.gz"),
            "00  archive.tar.gz\n"
        );
    }
}
