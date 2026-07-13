use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result, bail};

use crate::{
    artifacts::{AARCH64_TARGET, Aarch64Artifacts, Profile},
    cli::{Arch, Cli, Commands, LogLevel},
    initramfs, product, qemu,
};

const AARCH64_INITRAMFS_IMAGE_MAX_SIZE: u64 = 4 * 1024 * 1024;
const AARCH64_USER_CRT0: &str = "user/c/aarch64/crt0.S";
const AARCH64_USER_INCLUDE_DIR: &str = "user/c/aarch64/include";
const DEFAULT_INITRAMFS_ROOT: &str = "user/initramfs";

/// Dispatch one parsed xtask command.
///
/// # Arguments
///
/// * cli - Parsed command and its command-specific options.
///
/// # Returns
///
/// Returns success after the selected workflow completes.
///
/// # Errors
///
/// Returns an error when validation, a build tool, QEMU, or filesystem
/// operation fails.
pub(crate) fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Doctor => doctor(),
        Commands::Check => check(),
        Commands::TestAarch64 {
            case,
            list,
            profile,
            timeout_secs,
            artifacts_dir,
            keep_going,
        } => crate::test::run(crate::test::Options {
            case,
            list,
            profile,
            timeout_secs,
            artifacts_dir,
            keep_going,
            prepared: None,
        }),
        Commands::Ci => ci(),
        Commands::Dist { tag, output_dir } => crate::dist::run(&tag, &output_dir),
        Commands::Phase0Check => phase0_check(),
        Commands::RepoTree => repo_tree(),
        Commands::QemuCmd {
            arch,
            initramfs,
            profile,
        } => qemu_cmd(arch, initramfs, profile),
        Commands::GdbCmd { arch } => gdb_cmd(arch),
        Commands::BuildAarch64 { log_level, profile } => {
            build_aarch64(profile, log_level, &[]).map(|_| ())
        }
        Commands::BuildUserHello => build_user_program(Profile::Debug, "hello", false).map(|_| ()),
        Commands::BuildUserFault => {
            build_user_program(Profile::Debug, "fault_null", false).map(|_| ())
        }
        Commands::BuildUserReadFile => {
            build_user_program(Profile::Debug, "read_file", false).map(|_| ())
        }
        Commands::BuildUserShell => build_user_program(Profile::Debug, "shell", false).map(|_| ()),
        Commands::BuildUserEcho => build_user_program(Profile::Debug, "echo", false).map(|_| ()),
        Commands::BuildUserCat => build_user_program(Profile::Debug, "cat", false).map(|_| ()),
        Commands::BuildUserLs => build_user_program(Profile::Debug, "ls", false).map(|_| ()),
        Commands::BuildUserPwd => build_user_program(Profile::Debug, "pwd", false).map(|_| ()),
        Commands::BuildInitramfs {
            root,
            init,
            output,
            profile,
        } => build_initramfs(profile, root, init, output, false).map(|_| ()),
        Commands::RunAarch64 {
            log_level,
            initramfs,
            initramfs_root,
            init,
            profile,
        } => run_aarch64(false, profile, log_level, initramfs, initramfs_root, init),
        Commands::DebugAarch64 {
            log_level,
            initramfs,
            initramfs_root,
            init,
            profile,
        } => run_aarch64(
            true,
            profile,
            Some(log_level.unwrap_or(LogLevel::Debug)),
            initramfs,
            initramfs_root,
            init,
        ),
    }
}

fn doctor() -> Result<()> {
    let required = [
        "cargo",
        "rustup",
        "qemu-system-aarch64",
        "dtc",
        "fdtget",
        "fdtput",
        "clang",
        "ld.lld",
        "llvm-objdump",
        "readelf",
    ];
    let optional = ["just", "gdb", "aarch64-linux-gnu-gdb"];

    println!("== tool availability ==");
    let mut missing = Vec::new();

    for tool in required {
        match which(tool) {
            Some(path) => println!("[ok] {tool:<20} {path}"),
            None => {
                println!("[missing] {tool}");
                missing.push(tool);
            }
        }
    }
    for tool in optional {
        match which(tool) {
            Some(path) => println!("[optional ok] {tool:<20} {path}"),
            None => println!("[optional missing] {tool}"),
        }
    }

    println!("\n== rustup targets ==");
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .context("failed to query rustup targets")?;
    let installed = String::from_utf8_lossy(&output.stdout);
    let target = AARCH64_TARGET;
    let status = if installed.lines().any(|line| line.trim() == target) {
        "ok"
    } else {
        missing.push(target);
        "missing"
    };
    println!("[{status}] {target}");

    if missing.is_empty() {
        Ok(())
    } else {
        bail!("some required tools are missing")
    }
}

/// Run the repository's host, target, and production userspace checks.
///
/// This function takes no arguments and returns success only after formatting,
/// xtask tests/clippy, AArch64 linking/post-link verification, C builds, and
/// production initramfs creation complete.
///
/// # Errors
///
/// Returns the first command, build, or artifact-generation failure.
pub(crate) fn check() -> Result<()> {
    run_checked(
        Command::new("cargo").args(["fmt", "--all", "--", "--check"]),
        "cargo fmt",
    )?;
    run_checked(
        Command::new("cargo").args(["test", "-p", "xtask", "--locked"]),
        "xtask unit tests",
    )?;
    run_checked(
        Command::new("cargo").args([
            "clippy",
            "-p",
            "xtask",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ]),
        "xtask clippy",
    )?;

    let artifacts = build_aarch64(Profile::Debug, Some(LogLevel::Info), &[])?;
    crate::artifacts::verify_production_kernel(&artifacts.kernel_elf())?;
    let users = build_all_production_user_programs(Profile::Debug, true)?;
    crate::test::verify_product_contract_coverage(&users)?;
    build_initramfs_with_users(Profile::Debug, None, None, None, &users)?;
    Ok(())
}

fn ci() -> Result<()> {
    check()?;
    crate::test::run(crate::test::Options::ci_default())
}

fn phase0_check() -> Result<()> {
    let required_paths = [
        "AGENTS.md",
        "justfile",
        "rust-toolchain.toml",
        "ai-docs/decision-records/ADR-0001-architecture-strategy.md",
        "ai-docs/commits.md",
        "ai-docs/debugging.md",
        "tools/xtask/src/main.rs",
        "kernel/src/lib.rs",
        "crates/bootinfo/src/lib.rs",
        "arch/aarch64/src/boot.s",
        "arch/aarch64/link/qemu-virt.ld",
    ];

    println!("== phase 0 week 1 + week 2 checklist ==");
    for path in required_paths {
        ensure_exists(path)?;
        println!("[ok] {path}");
    }

    println!("\nPhase 0 / Week 1 scaffold and Week 2 AArch64 bring-up files are present.");
    Ok(())
}

fn repo_tree() -> Result<()> {
    println!("genrt/");
    for line in [
        "├── AGENTS.md",
        "├── Cargo.toml",
        "├── justfile",
        "├── rust-toolchain.toml",
        "├── kernel/",
        "├── arch/",
        "├── platform/",
        "├── crates/",
        "├── drivers/",
        "├── tools/xtask/",
        "├── tests/",
        "├── docs/",
        "└── ai-docs/",
    ] {
        println!("{line}");
    }
    Ok(())
}

fn qemu_cmd(arch: Arch, initramfs: Option<PathBuf>, profile: Profile) -> Result<()> {
    match arch {
        Arch::Aarch64 => {
            let artifacts = Aarch64Artifacts::new(profile);
            let initramfs = match initramfs {
                Some(path) => {
                    validate_initramfs_payload(&path)?;
                    path
                }
                None => artifacts.initramfs(),
            };
            let mut config = qemu::Config::from_artifacts(&artifacts, initramfs);
            config.wait_for_gdb = true;
            println!("{}", config.display());
        }
        Arch::X8664 => {
            println!("qemu-system-x86_64 -machine q35 -nographic -serial mon:stdio -S -s");
        }
        Arch::Riscv64 => {
            println!("qemu-system-riscv64 -machine virt -nographic -serial mon:stdio -S -s");
        }
    }
    Ok(())
}

fn gdb_cmd(arch: Arch) -> Result<()> {
    match arch {
        Arch::Aarch64 => {
            println!("aarch64-linux-gnu-gdb target/{AARCH64_TARGET}/debug/genrt-aarch64.elf");
            println!("(gdb) target remote :1234");
            println!("(gdb) break _start");
            println!("(gdb) break rust_entry");
            println!("(gdb) break kernel_main");
            println!("(gdb) continue");
        }
        Arch::X8664 => {
            println!("gdb target/x86_64-unknown-none/debug/kernel");
            println!("(gdb) target remote :1234");
        }
        Arch::Riscv64 => {
            println!("gdb target/riscv64gc-unknown-none-elf/debug/kernel");
            println!("(gdb) target remote :1234");
        }
    }
    Ok(())
}

/// Build and post-link one AArch64 kernel artifact set.
///
/// # Arguments
///
/// * profile - Cargo profile and artifact subtree.
/// * log_level - Optional compile-time kernel log level.
/// * extra_features - Additional architecture crate features, such as
///   test-only QEMU support.
///
/// # Returns
///
/// Returns canonical paths for the generated kernel ELF and DTB.
///
/// # Errors
///
/// Returns an error when DTB generation, Cargo, linking, or boot-text
/// verification fails.
pub(crate) fn build_aarch64(
    profile: Profile,
    log_level: Option<LogLevel>,
    extra_features: &[&str],
) -> Result<Aarch64Artifacts> {
    let artifacts = Aarch64Artifacts::new(profile);
    let dtb_path = generate_qemu_virt_dtb(&artifacts)?;

    let mut build = Command::new("cargo");
    build.args([
        "build",
        "--locked",
        "-p",
        "genrt-arch-aarch64",
        "--target",
        AARCH64_TARGET,
    ]);
    profile.apply_to_cargo(&mut build);
    build.env("GENRT_AARCH64_DTB_PATH", &dtb_path);

    let mut features = Vec::new();
    if let Some(level) = log_level {
        features.push(level.feature_name());
    }
    features.extend_from_slice(extra_features);
    if !features.is_empty() {
        build.args(["--features", &features.join(",")]);
    }

    let status = build.status().context("failed to invoke cargo build")?;

    if !status.success() {
        bail!("cargo build failed for genrt-arch-aarch64")
    }

    let final_elf = artifacts.kernel_elf();
    if let Some(parent) = final_elf.parent() {
        fs::create_dir_all(parent).context("failed to create output directory for final elf")?;
    }

    let staticlib = locate_staticlib(&artifacts)?;
    let link_status = Command::new("ld.lld")
        .args(["-T", "arch/aarch64/link/qemu-virt.ld", "-e", "_start", "-o"])
        .arg(&final_elf)
        .args(["--whole-archive"])
        .arg(&staticlib)
        .args(["--no-whole-archive"])
        .status()
        .context("failed to invoke ld.lld")?;

    if !link_status.success() {
        bail!("ld.lld failed to produce final AArch64 ELF")
    }

    verify_aarch64_boot_text_autonomy(&final_elf)?;
    println!("built {}", final_elf.display());
    Ok(artifacts)
}

fn run_aarch64(
    wait_for_gdb: bool,
    profile: Profile,
    log_level: Option<LogLevel>,
    initramfs: Option<PathBuf>,
    initramfs_root: Option<PathBuf>,
    init: Option<PathBuf>,
) -> Result<()> {
    let artifacts = build_aarch64(profile, log_level, &[])?;
    let initramfs = selected_or_build_initramfs_path(profile, initramfs, initramfs_root, init)?;
    let mut config = qemu::Config::from_artifacts(&artifacts, initramfs);
    config.wait_for_gdb = wait_for_gdb;
    let mut cmd = config.command();
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = cmd
        .status()
        .context("failed to invoke qemu-system-aarch64")?;
    if status.success() {
        Ok(())
    } else {
        bail!("QEMU exited with a non-zero status")
    }
}

fn selected_or_build_initramfs_path(
    profile: Profile,
    initramfs: Option<PathBuf>,
    root: Option<PathBuf>,
    init: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = initramfs {
        if root.is_some() || init.is_some() {
            bail!("--initramfs is mutually exclusive with --initramfs-root and --init");
        }
        validate_initramfs_payload(&path)?;
        return Ok(path);
    }

    build_initramfs(profile, root, init, None, false)
}

/// Build a deterministic newc CPIO initramfs.
///
/// # Arguments
///
/// * profile - Artifact profile used for userspace binaries and defaults.
/// * root - Optional source tree; the repository sample root is the default.
/// * init - Optional prebuilt ELF staged as /init.
/// * output - Optional archive path; the profile default is used otherwise.
/// * warnings_as_errors - Whether C compilation adds -Werror.
///
/// # Returns
///
/// Returns the validated output archive path.
///
/// # Errors
///
/// Returns an error for invalid input paths, failed user builds, unsupported
/// archive entries, I/O failures, or an oversized/empty archive.
pub(crate) fn build_initramfs(
    profile: Profile,
    root: Option<PathBuf>,
    init: Option<PathBuf>,
    output: Option<PathBuf>,
    warnings_as_errors: bool,
) -> Result<PathBuf> {
    let users = build_production_user_artifacts(profile, warnings_as_errors)?;
    build_initramfs_with_users(profile, root, init, output, &users)
}

#[derive(Clone)]
/// Exact production userspace ELF paths reused across test and release images.
pub(crate) struct ProductionUserArtifacts {
    programs: Vec<BuiltProductionProgram>,
}

#[derive(Clone)]
/// One built product program paired with its declarative metadata.
pub(crate) struct BuiltProductionProgram {
    metadata: product::Program,
    elf: PathBuf,
}

impl ProductionUserArtifacts {
    /// Return the production shell ELF used as release `/init`.
    ///
    /// # Returns
    ///
    /// Returns the already-built shell artifact path.
    pub(crate) fn shell(&self) -> &Path {
        self.programs
            .iter()
            .find(|program| program.metadata.install() == "init")
            .map(|program| program.elf.as_path())
            .expect("validated product manifest always provides init")
    }

    /// Return every built program in product-manifest order.
    ///
    /// # Returns
    ///
    /// Returns a borrowed slice without allocation.
    pub(crate) fn programs(&self) -> &[BuiltProductionProgram] {
        &self.programs
    }
}

impl BuiltProductionProgram {
    /// Return the product-manifest program name.
    pub(crate) fn name(&self) -> &str {
        self.metadata.name()
    }

    /// Return the release initramfs install path.
    pub(crate) fn install(&self) -> &str {
        self.metadata.install()
    }

    /// Return the dynamic contract role or `structural`.
    pub(crate) fn contract(&self) -> &str {
        self.metadata.contract()
    }

    /// Return the install path used by the selected contract image.
    pub(crate) fn contract_install(&self) -> &str {
        self.metadata.contract_install()
    }

    /// Return the protocol case proving dynamic contract coverage.
    pub(crate) fn contract_case(&self) -> Option<&str> {
        self.metadata.contract_case()
    }

    /// Return the already-built ELF path.
    pub(crate) fn elf(&self) -> &Path {
        &self.elf
    }
}

/// Build the complete production userspace executable set once.
///
/// # Arguments
///
/// * `profile` - Artifact profile shared by all programs.
/// * `warnings_as_errors` - Whether Clang warnings fail the build.
///
/// # Returns
///
/// Returns paths to the shell and release `/bin` programs.
///
/// # Errors
///
/// Returns an error when any compile, link, validation, or filesystem step
/// fails.
pub(crate) fn build_production_user_artifacts(
    profile: Profile,
    warnings_as_errors: bool,
) -> Result<ProductionUserArtifacts> {
    let metadata = product::load()?;
    let mut programs = Vec::new();
    programs.try_reserve_exact(metadata.len())?;
    for program in metadata {
        let elf = build_aarch64_user_elf(
            program.name(),
            program.source(),
            Aarch64Artifacts::new(profile).user_elf(program.name()),
            warnings_as_errors,
            &[],
            false,
        )?;
        programs.push(BuiltProductionProgram {
            metadata: program,
            elf,
        });
    }
    Ok(ProductionUserArtifacts { programs })
}

/// Build an initramfs from a previously built production userspace set.
///
/// # Arguments
///
/// * `profile` - Selects staging and default output paths.
/// * `root` - Optional source tree; the repository production root is default.
/// * `init` - Optional prebuilt `/init`; the production shell is default.
/// * `output` - Optional archive path.
/// * `users` - Exact production ELF files to stage without rebuilding.
///
/// # Returns
///
/// Returns the generated and structurally verified CPIO path.
///
/// # Errors
///
/// Returns an error for invalid inputs, staging conflicts, filesystem errors,
/// oversized output, or manifest/archive verification failures.
pub(crate) fn build_initramfs_with_users(
    profile: Profile,
    root: Option<PathBuf>,
    init: Option<PathBuf>,
    output: Option<PathBuf>,
    users: &ProductionUserArtifacts,
) -> Result<PathBuf> {
    build_initramfs_with_provenance(
        profile,
        root,
        init,
        output,
        users,
        &initramfs::Provenance::default(),
        initramfs::Policy::Production,
    )
}

/// Build a test initramfs while recording explicit entry provenance.
///
/// # Arguments
///
/// * `profile` - Selects staging and default output paths.
/// * `root` - Optional source tree.
/// * `init` - Optional prebuilt `/init` executable.
/// * `output` - Optional destination archive path.
/// * `users` - Exact product-manifest ELF artifacts.
/// * `provenance` - Origin prefixes for test-only staged entries.
///
/// # Returns
///
/// Returns the generated CPIO path verified under the test-content policy.
///
/// # Errors
///
/// Returns an error for invalid inputs, staging conflicts, I/O failures,
/// oversized output, or manifest/archive verification failures.
pub(crate) fn build_test_initramfs_with_users(
    profile: Profile,
    root: Option<PathBuf>,
    init: Option<PathBuf>,
    output: Option<PathBuf>,
    users: &ProductionUserArtifacts,
    provenance: &initramfs::Provenance,
) -> Result<PathBuf> {
    build_initramfs_with_provenance(
        profile,
        root,
        init,
        output,
        users,
        provenance,
        initramfs::Policy::Test,
    )
}

fn build_initramfs_with_provenance(
    profile: Profile,
    root: Option<PathBuf>,
    init: Option<PathBuf>,
    output: Option<PathBuf>,
    users: &ProductionUserArtifacts,
    provenance: &initramfs::Provenance,
    policy: initramfs::Policy,
) -> Result<PathBuf> {
    let root = root.unwrap_or_else(|| PathBuf::from(DEFAULT_INITRAMFS_ROOT));
    ensure_dir(&root)?;

    let init = match init {
        Some(path) => {
            validate_user_elf_payload(&path)?;
            path
        }
        None => users.shell().to_path_buf(),
    };

    let artifacts = Aarch64Artifacts::new(profile);
    let output = output.unwrap_or_else(|| artifacts.initramfs());
    let staging = artifacts.initramfs_staging();
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .with_context(|| format!("failed to clear {}", staging.display()))?;
    }
    fs::create_dir_all(&staging)
        .with_context(|| format!("failed to create {}", staging.display()))?;

    copy_initramfs_root(&root, &staging)?;
    let staged_init = staging.join("init");
    if staged_init.exists() {
        bail!(
            "initramfs root {} already contains init; pass a root without init",
            root.display()
        );
    }
    fs::copy(&init, &staged_init).with_context(|| {
        format!(
            "failed to stage init ELF {} as {}",
            init.display(),
            staged_init.display()
        )
    })?;
    for program in users.programs() {
        if program.install() != "init" {
            stage_elf(&root, &staging, program.install(), program.elf())?;
        }
    }

    initramfs::build(&staging, &output, policy, provenance)?;
    let size = validate_initramfs_payload(&output)?;
    println!("built {} ({} bytes)", output.display(), size);
    Ok(output)
}

/// Stage product programs assigned to one dynamic contract role.
///
/// # Arguments
///
/// * `root` - Contract initramfs staging root.
/// * `contract` - Role selected from the product manifest.
/// * `users` - Exact already-built production artifacts.
///
/// # Returns
///
/// Returns success after copying every matching program.
///
/// # Errors
///
/// Returns an error for destination conflicts or filesystem failures.
pub(crate) fn stage_contract_programs(
    root: &Path,
    contract: &str,
    users: &ProductionUserArtifacts,
) -> Result<()> {
    for program in users
        .programs()
        .iter()
        .filter(|program| program.contract() == contract)
    {
        stage_elf(root, root, program.contract_install(), program.elf())?;
    }
    Ok(())
}

fn stage_elf(root: &Path, staging: &Path, install: &str, elf: &Path) -> Result<()> {
    let staged = staging.join(install);
    if staged.exists() {
        bail!(
            "initramfs root {} already contains {}; remove it or update the product manifest",
            root.display(),
            install
        );
    }
    if let Some(parent) = staged.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(elf, &staged).with_context(|| {
        format!(
            "failed to stage product ELF {} as {}",
            elf.display(),
            staged.display()
        )
    })?;
    Ok(())
}

fn copy_initramfs_root(src: &Path, dst: &Path) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry.with_context(|| format!("failed to read entry in {}", src.display()))?;
        let source = entry.path();
        let target = dst.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source)
            .with_context(|| format!("failed to stat {}", source.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("initramfs rejects symlink {}", source.display());
        }
        if metadata.is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create {}", target.display()))?;
            copy_initramfs_root(&source, &target)?;
        } else if metadata.is_file() {
            fs::copy(&source, &target).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source.display(),
                    target.display()
                )
            })?;
        } else {
            bail!(
                "initramfs rejects unsupported file type {}",
                source.display()
            );
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct UserProgram {
    name: &'static str,
    source: &'static str,
    checked: bool,
    defines: &'static [&'static str],
    test_artifact: bool,
}

const USER_PROGRAMS: &[UserProgram] = &[
    UserProgram {
        name: "hello",
        source: "user/c/hello.c",
        checked: true,
        defines: &[],
        test_artifact: false,
    },
    UserProgram {
        name: "fault_null",
        source: "user/c/fault_null.c",
        checked: true,
        defines: &[],
        test_artifact: false,
    },
    UserProgram {
        name: "read_file",
        source: "user/c/read_file.c",
        checked: true,
        defines: &[],
        test_artifact: false,
    },
    UserProgram {
        name: "test_api_case",
        source: "tests/qemu/user/api_case.c",
        checked: false,
        defines: &[],
        test_artifact: true,
    },
    UserProgram {
        name: "test_api_supervisor",
        source: "tests/qemu/user/api_supervisor.c",
        checked: false,
        defines: &[],
        test_artifact: true,
    },
    UserProgram {
        name: "test_shell_supervisor",
        source: "tests/qemu/user/shell_supervisor.c",
        checked: false,
        defines: &[],
        test_artifact: true,
    },
    UserProgram {
        name: "test_shell_probe",
        source: "tests/qemu/user/shell_probe.c",
        checked: false,
        defines: &[],
        test_artifact: true,
    },
];

/// Build one named userspace program from the repository manifest.
///
/// # Arguments
///
/// * profile - Artifact profile and output subtree.
/// * name - Manifest program name.
/// * warnings_as_errors - Whether Clang treats warnings as errors.
///
/// Program-specific preprocessor definitions are read from the internal
/// manifest and are not supplied by callers.
///
/// # Returns
///
/// Returns the validated ELF output path.
///
/// # Errors
///
/// Returns an error for an unknown program, missing source/tool, failed
/// compile/link command, or empty output.
pub(crate) fn build_user_program(
    profile: Profile,
    name: &str,
    warnings_as_errors: bool,
) -> Result<PathBuf> {
    if let Some(program) = product::load()?
        .into_iter()
        .find(|program| program.name() == name)
    {
        return build_aarch64_user_elf(
            program.name(),
            program.source(),
            Aarch64Artifacts::new(profile).user_elf(program.name()),
            warnings_as_errors,
            &[],
            false,
        );
    }
    let program = USER_PROGRAMS
        .iter()
        .find(|program| program.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown userspace program {name}"))?;
    build_aarch64_user_elf(
        program.name,
        Path::new(program.source),
        Aarch64Artifacts::new(profile).user_elf(program.name),
        warnings_as_errors,
        program.defines,
        program.test_artifact,
    )
}

fn build_all_production_user_programs(
    profile: Profile,
    warnings_as_errors: bool,
) -> Result<ProductionUserArtifacts> {
    let users = build_production_user_artifacts(profile, warnings_as_errors)?;
    for program in USER_PROGRAMS.iter().filter(|program| program.checked) {
        build_user_program(profile, program.name, warnings_as_errors)?;
    }
    Ok(users)
}

fn build_aarch64_user_elf(
    name: &str,
    source: &Path,
    elf: PathBuf,
    warnings_as_errors: bool,
    defines: &[&str],
    test_artifact: bool,
) -> Result<PathBuf> {
    ensure_exists(source.to_str().unwrap_or("<invalid user source path>"))?;
    ensure_exists(AARCH64_USER_CRT0)?;
    ensure_exists(AARCH64_USER_INCLUDE_DIR)?;
    ensure_exists("user/c/linker.ld")?;

    let parent = elf
        .parent()
        .ok_or_else(|| anyhow::anyhow!("user ELF path has no parent: {}", elf.display()))?;
    fs::create_dir_all(parent).context("failed to create user ELF output directory")?;

    let crt_obj = parent.join(format!("{name}.crt0.o"));
    let obj = parent.join(format!("{name}.o"));

    let mut crt_compile = Command::new("clang");
    crt_compile
        .args(aarch64_user_compile_args())
        .arg("-c")
        .arg(AARCH64_USER_CRT0)
        .arg("-o")
        .arg(&crt_obj);
    if warnings_as_errors {
        crt_compile.arg("-Werror");
    }
    let status = crt_compile
        .status()
        .with_context(|| format!("failed to invoke clang for AArch64 user crt0 {name}"))?;
    if !status.success() {
        bail!("clang failed to assemble AArch64 user crt0 for {name}")
    }

    let mut source_compile = Command::new("clang");
    source_compile
        .args(aarch64_user_compile_args())
        .arg("-c")
        .arg(source)
        .arg("-o")
        .arg(&obj);
    if test_artifact {
        source_compile.args(["-include", "tests/qemu/user/artifact_marker.h"]);
    }
    let contract_role = match name {
        "test_api_supervisor" => Some("userspace"),
        "test_shell_supervisor" => Some("shell"),
        _ => None,
    };
    if let Some(contract_role) = contract_role {
        let include_dir = parent.join(format!("{name}-generated"));
        crate::product_contract::Plan::load()?.write_c_header(&include_dir, contract_role)?;
        source_compile.arg("-I").arg(include_dir);
    }
    for define in defines {
        source_compile.arg(format!("-D{define}"));
    }
    if warnings_as_errors {
        source_compile.arg("-Werror");
    }
    let status = source_compile
        .status()
        .with_context(|| format!("failed to invoke clang for AArch64 user program {name}"))?;
    if !status.success() {
        bail!("clang failed to compile AArch64 user program {name}")
    }

    let status = Command::new("ld.lld")
        .args([
            "-T",
            "user/c/linker.ld",
            "--build-id=none",
            "-z",
            "max-page-size=0x1000",
            "-z",
            "common-page-size=0x1000",
            "-o",
        ])
        .arg(&elf)
        .arg(&crt_obj)
        .arg(&obj)
        .status()
        .with_context(|| format!("failed to invoke ld.lld for AArch64 user program {name}"))?;
    if !status.success() {
        bail!("ld.lld failed to link AArch64 user ELF {name}")
    }

    let size = validate_user_elf_payload(&elf)?;

    println!("built {} ({} bytes)", elf.display(), size);
    Ok(elf)
}

fn validate_user_elf_payload(path: &Path) -> Result<u64> {
    let size = fs::metadata(path)
        .with_context(|| format!("failed to stat user ELF {}", path.display()))?
        .len();
    if size == 0 {
        bail!("user ELF {} is empty", path.display());
    }
    Ok(size)
}

fn validate_initramfs_payload(path: &Path) -> Result<u64> {
    let size = fs::metadata(path)
        .with_context(|| format!("failed to stat initramfs {}", path.display()))?
        .len();
    if size == 0 {
        bail!("initramfs {} is empty", path.display());
    }
    if size > AARCH64_INITRAMFS_IMAGE_MAX_SIZE {
        bail!(
            "initramfs {} size {} exceeds loader reservation {} bytes",
            path.display(),
            size,
            AARCH64_INITRAMFS_IMAGE_MAX_SIZE
        );
    }
    Ok(size)
}

fn aarch64_user_compile_args() -> [&'static str; 13] {
    [
        "--target=aarch64-none-elf",
        "-ffreestanding",
        "-fno-builtin",
        "-fno-stack-protector",
        "-fno-pic",
        "-fno-unwind-tables",
        "-fno-asynchronous-unwind-tables",
        "-mgeneral-regs-only",
        "-nostdlib",
        "-I",
        AARCH64_USER_INCLUDE_DIR,
        "-Wall",
        "-Wextra",
    ]
}

fn generate_qemu_virt_dtb(artifacts: &Aarch64Artifacts) -> Result<PathBuf> {
    let dtb_path = env::current_dir()
        .context("failed to query current directory")?
        .join(artifacts.dtb());
    if let Some(parent) = dtb_path.parent() {
        fs::create_dir_all(parent).context("failed to create DTB output directory")?;
    }

    let status = Command::new("qemu-system-aarch64")
        .arg("-machine")
        .arg(format!("{},dumpdtb={}", qemu::MACHINE, dtb_path.display()))
        .args([
            "-cpu",
            qemu::CPU,
            "-display",
            "none",
            "-serial",
            "null",
            "-monitor",
            "none",
            "-nic",
            "none",
        ])
        .status()
        .context("failed to invoke qemu-system-aarch64 for DTB generation")?;

    if !status.success() {
        bail!("qemu-system-aarch64 failed to generate virt DTB")
    }

    remove_nondeterministic_dtb_seeds(&dtb_path)?;
    compact_dtb(&dtb_path)?;
    trim_dtb_to_fdt_totalsize(&dtb_path)?;
    Ok(dtb_path)
}

fn remove_nondeterministic_dtb_seeds(path: &Path) -> Result<()> {
    for property in ["rng-seed", "kaslr-seed"] {
        let present = Command::new("fdtget")
            .arg(path)
            .args(["/chosen", property])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| {
                format!("failed to inspect /chosen/{property} in {}", path.display())
            })?;
        if !present.success() {
            continue;
        }
        let status = Command::new("fdtput")
            .args(["-d"])
            .arg(path)
            .args(["/chosen", property])
            .status()
            .with_context(|| {
                format!(
                    "failed to invoke fdtput for /chosen/{property} in {}",
                    path.display()
                )
            })?;
        if !status.success() {
            bail!(
                "fdtput failed to remove /chosen/{property} from {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn compact_dtb(path: &Path) -> Result<()> {
    let compact_path = path.with_extension("compact.dtb");
    let status = Command::new("dtc")
        .args(["-I", "dtb", "-O", "dtb", "-o"])
        .arg(&compact_path)
        .arg(path)
        .status()
        .context("failed to invoke dtc to compact generated DTB")?;
    if !status.success() {
        bail!("dtc failed to compact generated DTB")
    }

    fs::rename(&compact_path, path).with_context(|| {
        format!(
            "failed to replace {} with compacted DTB {}",
            path.display(),
            compact_path.display()
        )
    })?;
    Ok(())
}

fn trim_dtb_to_fdt_totalsize(path: &Path) -> Result<()> {
    let header = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if header.len() < 8 {
        bail!("generated DTB is too small: {}", path.display());
    }

    let magic = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    if magic != 0xd00d_feed {
        bail!("generated DTB has invalid FDT magic: {}", path.display());
    }

    let total_size = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as u64;
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open {} for trimming", path.display()))?
        .set_len(total_size)
        .with_context(|| format!("failed to trim {}", path.display()))?;
    Ok(())
}

fn locate_staticlib(artifacts: &Aarch64Artifacts) -> Result<PathBuf> {
    let direct = artifacts.staticlib();
    if direct.exists() {
        return Ok(direct);
    }

    let deps_buf = artifacts.root().join("deps");
    let deps = deps_buf.as_path();
    if deps.is_dir() {
        let mut candidates = Vec::new();
        for entry in fs::read_dir(deps).context("failed to scan target deps directory")? {
            let entry = entry.context("failed to read target deps entry")?;
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|s| s.to_str())
                && name.starts_with("libgenrt_arch_aarch64")
                && name.ends_with(".a")
            {
                candidates.push(path);
            }
        }
        candidates.sort();
        if let Some(last) = candidates.pop() {
            return Ok(last);
        }
    }

    bail!("unable to locate libgenrt_arch_aarch64.a after cargo build")
}

fn verify_aarch64_boot_text_autonomy(elf: &Path) -> Result<()> {
    let sections = read_elf_sections(elf)?;
    let allowed_ranges = [".boot.text", ".boot.rodata", ".boot.bss", ".boot_stack"]
        .into_iter()
        .filter_map(|name| sections.iter().find(|section| section.name == name))
        .map(|section| (section.addr, section.end()))
        .collect::<Vec<_>>();

    if allowed_ranges.is_empty() {
        bail!("AArch64 boot autonomy check failed: no .boot.* sections found")
    }

    ensure_no_boot_text_relocations(elf)?;
    let disassembly = command_stdout(
        Command::new("llvm-objdump")
            .args(["-dr", "--section=.boot.text"])
            .arg(elf),
        "failed to disassemble .boot.text with llvm-objdump",
    )?;

    let mut violations = Vec::new();
    let forbidden = [
        "__AArch64AbsLongThunk",
        "memcpy",
        "memset",
        "memmove",
        "compiler_builtins",
        "panic",
        "panicking",
        "core::fmt",
        "fmt::",
        "log::",
    ];

    for line in disassembly.lines() {
        if !is_objdump_code_or_symbol_line(line) {
            continue;
        }

        if forbidden.iter().any(|needle| line.contains(needle)) {
            violations.push(format!(
                "forbidden runtime/helper symbol in .boot.text: {line}"
            ));
        }

        if objdump_instruction(line).is_some_and(|instruction| instruction.contains("0xffff0000")) {
            violations.push(format!(
                ".boot.text instruction references high VA before MMU is enabled: {line}"
            ));
        }

        let Some(target) = direct_branch_target(line) else {
            continue;
        };
        if !addr_in_ranges(target, &allowed_ranges) {
            violations.push(format!(
                ".boot.text direct branch/call leaves boot sections: target=0x{target:x}; {line}"
            ));
        }
    }

    if !violations.is_empty() {
        for violation in &violations {
            eprintln!("[boot-text] {violation}");
        }
        bail!(
            "AArch64 boot autonomy check failed: .boot.text references code/data outside .boot.*"
        );
    }

    println!(
        "verified .boot.text autonomy: no relocations, no runtime thunks, no high-VA instruction operands, direct branches stay in .boot.*"
    );
    Ok(())
}

fn ensure_no_boot_text_relocations(elf: &Path) -> Result<()> {
    let relocations = command_stdout(
        Command::new("readelf").args(["-rW"]).arg(elf),
        "failed to inspect ELF relocations with readelf",
    )?;
    let mut in_boot_rela = false;
    let mut violations = Vec::new();

    for line in relocations.lines() {
        if line.starts_with("Relocation section ") {
            in_boot_rela = line.contains(".rela.boot.text") || line.contains(".rel.boot.text");
            continue;
        }
        if in_boot_rela && !line.trim().is_empty() && !line.contains("Offset") {
            violations.push(line.to_string());
        }
    }

    if !violations.is_empty() {
        for violation in &violations {
            eprintln!("[boot-text] relocation remains in .boot.text: {violation}");
        }
        bail!("AArch64 boot autonomy check failed: .boot.text has relocations");
    }

    Ok(())
}

#[derive(Debug)]
struct ElfSection {
    name: String,
    addr: u64,
    size: u64,
}

impl ElfSection {
    fn end(&self) -> u64 {
        self.addr.saturating_add(self.size)
    }
}

fn read_elf_sections(elf: &Path) -> Result<Vec<ElfSection>> {
    let output = command_stdout(
        Command::new("readelf").args(["-SW"]).arg(elf),
        "failed to inspect ELF sections with readelf",
    )?;
    let mut sections = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('[') {
            continue;
        }

        let fields = trimmed.split_whitespace().collect::<Vec<_>>();
        if fields.first() == Some(&"[Nr]") {
            continue;
        }
        let base = if fields.first() == Some(&"[") { 1 } else { 0 };
        if fields.len() < base + 6 {
            continue;
        }

        let name = fields[base + 1].to_string();
        let addr = u64::from_str_radix(fields[base + 3], 16)
            .with_context(|| format!("failed to parse section address from line: {line}"))?;
        let size = u64::from_str_radix(fields[base + 5], 16)
            .with_context(|| format!("failed to parse section size from line: {line}"))?;
        sections.push(ElfSection { name, addr, size });
    }

    Ok(sections)
}

fn is_objdump_code_or_symbol_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed.ends_with(">:") {
        return trimmed
            .split_once(' ')
            .is_some_and(|(addr, _)| u64::from_str_radix(addr, 16).is_ok());
    }

    trimmed
        .split_once(':')
        .is_some_and(|(addr, _)| u64::from_str_radix(addr.trim(), 16).is_ok())
}

fn direct_branch_target(line: &str) -> Option<u64> {
    let instruction = objdump_instruction(line)?;
    let mnemonic = instruction.split_whitespace().next()?;
    let is_direct_branch = mnemonic == "b"
        || mnemonic == "bl"
        || mnemonic.starts_with("b.")
        || mnemonic == "cbz"
        || mnemonic == "cbnz"
        || mnemonic == "tbz"
        || mnemonic == "tbnz";
    if !is_direct_branch {
        return None;
    }

    for token in instruction.split(|ch: char| ch.is_whitespace() || ch == ',' || ch == '<') {
        let Some(hex) = token.strip_prefix("0x") else {
            continue;
        };
        let hex = hex.trim_end_matches(|ch: char| !ch.is_ascii_hexdigit());
        if let Ok(value) = u64::from_str_radix(hex, 16) {
            return Some(value);
        }
    }

    None
}

fn objdump_instruction(line: &str) -> Option<&str> {
    let after_addr = line.split_once(':')?.1.trim_start();
    let (encoding, instruction) = after_addr.split_once(char::is_whitespace)?;
    if encoding.len() == 8 && encoding.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(instruction.trim_start())
    } else {
        None
    }
}

fn addr_in_ranges(addr: u64, ranges: &[(u64, u64)]) -> bool {
    ranges
        .iter()
        .any(|(start, end)| addr >= *start && addr < *end)
}

fn command_stdout(command: &mut Command, context: &str) -> Result<String> {
    let Output {
        status,
        stdout,
        stderr,
    } = command.output().with_context(|| context.to_string())?;
    if !status.success() {
        bail!("{context}: {}", String::from_utf8_lossy(&stderr));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

fn run_checked(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to invoke {description}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("{description} failed with status {status}")
    }
}

fn ensure_exists(path: &str) -> Result<()> {
    if Path::new(path).exists() {
        Ok(())
    } else {
        bail!("missing required path: {path}")
    }
}

fn ensure_dir(path: &Path) -> Result<()> {
    if path.is_dir() {
        Ok(())
    } else {
        bail!("missing required directory: {}", path.display())
    }
}

fn which(tool: &str) -> Option<String> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.display().to_string())
}
