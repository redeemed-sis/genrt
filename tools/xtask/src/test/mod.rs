mod cases;
mod protocol;
mod runner;

use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::{
    artifacts::{Aarch64Artifacts, Profile},
    cli::LogLevel,
    qemu, workflow,
};

/// Options controlling selection, timeouts, logs, and prepared artifacts.
pub(crate) struct Options {
    /// Optional exact case name; all cases run when absent.
    pub(crate) case: Option<String>,
    /// Print case names without building or running QEMU.
    pub(crate) list: bool,
    /// Optional CLI profile override; each case declares its default.
    pub(crate) profile: Option<Profile>,
    /// Per-case QEMU runtime bound and unit of aggregate runtime accounting.
    pub(crate) timeout_secs: u64,
    /// Root directory for serial logs and JSON results.
    pub(crate) artifacts_dir: PathBuf,
    /// Continue with later cases after a failure.
    pub(crate) keep_going: bool,
    /// Exact production kernel and contract image supplied without rebuilding.
    pub(crate) prepared: Option<PreparedArtifacts>,
}

impl Options {
    /// Return options for the full local/hosted CI QEMU suite.
    ///
    /// The returned configuration selects all debug cases, a 60-second case
    /// ceiling, fail-fast behavior, and target/test-results output.
    pub(crate) fn ci_default() -> Self {
        Self {
            case: None,
            list: false,
            profile: None,
            timeout_secs: 60,
            artifacts_dir: PathBuf::from("target/test-results"),
            keep_going: false,
            prepared: None,
        }
    }
}

/// Exact already-built files supplied to production contract cases.
#[derive(Clone)]
pub(crate) struct PreparedArtifacts {
    /// Kernel ELF and DTB paths for one profile.
    pub(crate) artifacts: Aarch64Artifacts,
    /// Initramfs file tested and later packaged without rebuilding.
    pub(crate) initramfs: PathBuf,
}

#[derive(Serialize)]
struct Summary {
    status: &'static str,
    duration_ms: u128,
    cases: Vec<runner::CaseResult>,
}

/// Run or list declarative AArch64 QEMU cases.
///
/// # Arguments
///
/// * options - Case selection, timeout, output, and optional prepared files.
///
/// # Returns
///
/// Returns success when listing completes or every selected case passes.
///
/// # Errors
///
/// Returns an error for invalid case files/selections, build failures, QEMU
/// spawn or timeout failures, forbidden/missing markers, or result I/O errors.
pub(crate) fn run(options: Options) -> Result<()> {
    let all = cases::load_all(PathBuf::from("tests/qemu/cases").as_path())?;
    if options.list {
        for case in &all {
            println!("{}", case.name);
        }
        return Ok(());
    }

    let mut selected = all
        .into_iter()
        .filter(|case| options.case.as_ref().is_none_or(|name| name == &case.name))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("unknown QEMU case {:?}", options.case);
    }
    selected.sort_by_key(|case| case.kernel_features.is_empty());

    fs::create_dir_all(&options.artifacts_dir).with_context(|| {
        format!(
            "failed to create test artifact directory {}",
            options.artifacts_dir.display()
        )
    })?;
    let suite_started = Instant::now();
    let suite_runtime_budget =
        Duration::from_secs(options.timeout_secs.saturating_mul(selected.len() as u64));
    let mut suite_runtime_used = Duration::ZERO;
    let mut results = Vec::new();
    let mut failed = false;
    let mut production_users = Vec::new();
    let mut last_kernel: Option<(Profile, String, Vec<String>, Aarch64Artifacts)> = None;

    for mut case in selected {
        let profile = options.profile.unwrap_or(case.profile);
        case.profile = profile;
        let users = if options.prepared.is_none() {
            let index = match production_users
                .iter()
                .position(|(existing, _)| *existing == profile)
            {
                Some(index) => index,
                None => {
                    production_users.push((
                        profile,
                        workflow::build_production_user_artifacts(profile, true)?,
                    ));
                    production_users.len() - 1
                }
            };
            Some(&production_users[index].1)
        } else {
            None
        };
        let built_kernel = if options.prepared.is_none() {
            let reusable =
                last_kernel
                    .as_ref()
                    .is_some_and(|(last_profile, log_level, features, _)| {
                        *last_profile == profile
                            && log_level == &case.log_level
                            && features == &case.kernel_features
                    });
            if !reusable {
                let features = case
                    .kernel_features
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                let log_level = parse_log_level(&case.log_level)?;
                let artifacts = workflow::build_aarch64(profile, Some(log_level), &features)?;
                last_kernel = Some((
                    profile,
                    case.log_level.clone(),
                    case.kernel_features.clone(),
                    artifacts,
                ));
            }
            last_kernel
                .as_ref()
                .map(|(_, _, _, artifacts)| artifacts.clone())
        } else {
            None
        };
        let (artifacts, initramfs) = prepare_case(
            &case,
            profile,
            options.prepared.as_ref(),
            users,
            built_kernel,
        )?;
        let config = qemu::Config::from_artifacts(&artifacts, initramfs);
        let case_dir = options.artifacts_dir.join(&case.name);
        let remaining_runtime = suite_runtime_budget.saturating_sub(suite_runtime_used);
        if remaining_runtime.is_zero() {
            bail!("QEMU runtime budget expired before case {}", case.name);
        }
        let result = runner::run_case(
            &case,
            &config,
            &case_dir,
            options.timeout_secs,
            Instant::now() + remaining_runtime,
        )?;
        let result_ms = u64::try_from(result.duration_ms).unwrap_or(u64::MAX);
        suite_runtime_used = suite_runtime_used.saturating_add(Duration::from_millis(result_ms));
        runner::write_result(case_dir.join("result.json"), &result)?;
        if runner::report_failure(&result, &case_dir).is_err() {
            failed = true;
        } else {
            println!("PASS {} ({} ms)", result.name, result.duration_ms);
        }
        results.push(result);
        if failed && !options.keep_going {
            break;
        }
    }

    let summary = Summary {
        status: if failed { "failed" } else { "passed" },
        duration_ms: suite_started.elapsed().as_millis(),
        cases: results,
    };
    fs::write(
        options.artifacts_dir.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    if failed {
        bail!("one or more QEMU cases failed")
    }
    Ok(())
}

fn prepare_case(
    case: &cases::Case,
    profile: Profile,
    prepared: Option<&PreparedArtifacts>,
    users: Option<&workflow::ProductionUserArtifacts>,
    built_kernel: Option<Aarch64Artifacts>,
) -> Result<(Aarch64Artifacts, PathBuf)> {
    if let Some(prepared) = prepared {
        if !case.kernel_features.is_empty() {
            bail!(
                "prepared production kernel cannot run feature test {}",
                case.name
            );
        }
        return Ok((prepared.artifacts.clone(), prepared.initramfs.clone()));
    }

    let artifacts = built_kernel.ok_or_else(|| anyhow::anyhow!("missing kernel artifacts"))?;
    let output = artifacts
        .root()
        .join(format!("{}.initramfs.cpio", case.name));
    let users = users.ok_or_else(|| anyhow::anyhow!("missing production userspace artifacts"))?;
    let initramfs = prepare_test_initramfs(profile, &case.init, output, users)?;
    Ok((artifacts, initramfs))
}

fn prepare_test_initramfs(
    profile: Profile,
    image: &cases::InitImage,
    output: PathBuf,
    users: &workflow::ProductionUserArtifacts,
) -> Result<PathBuf> {
    let artifacts = Aarch64Artifacts::new(profile);
    let root = artifacts.root().join("qemu-test-root").join(match image {
        cases::InitImage::KernelContract => "kernel-contract",
        cases::InitImage::UserFault => "user-fault",
        cases::InitImage::UserspaceContract => "userspace-contract",
        cases::InitImage::ShellContract => "shell-contract",
    });
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(&root)?;
    let test_namespace = root.join(".__genrt_test__");
    let mut provenance = crate::initramfs::Provenance::default();
    provenance.mark_prefix(".__genrt_test__", crate::initramfs::Origin::TestFixture)?;

    let init = match image {
        cases::InitImage::KernelContract => workflow::build_user_program(profile, "hello", true)?,
        cases::InitImage::UserFault => workflow::build_user_program(profile, "fault_null", true)?,
        cases::InitImage::UserspaceContract => {
            copy_tree(
                PathBuf::from("tests/qemu/fixtures/initramfs/fixtures").as_path(),
                &test_namespace.join("fixtures"),
            )?;
            let test_dir = test_namespace.join("bin");
            fs::create_dir_all(&test_dir)?;
            let api_case = workflow::build_user_program(profile, "test_api_case", true)?;
            fs::copy(api_case, test_dir.join("api-case"))?;
            provenance.mark_prefix("init", crate::initramfs::Origin::TestSupervisor)?;
            workflow::build_user_program(profile, "test_api_supervisor", true)?
        }
        cases::InitImage::ShellContract => {
            copy_tree(
                PathBuf::from("tests/qemu/fixtures/initramfs/fixtures").as_path(),
                &test_namespace.join("fixtures"),
            )?;
            let test_dir = test_namespace.join("bin");
            fs::create_dir_all(&test_dir)?;
            let probe = workflow::build_user_program(profile, "test_shell_probe", true)?;
            fs::copy(probe, test_dir.join("p"))?;
            workflow::stage_contract_programs(&root, "shell", users)?;
            provenance.mark_prefix("init", crate::initramfs::Origin::TestSupervisor)?;
            workflow::build_user_program(profile, "test_shell_supervisor", true)?
        }
    };
    if !matches!(
        image,
        cases::InitImage::UserspaceContract | cases::InitImage::ShellContract
    ) {
        provenance.mark_prefix("init", crate::initramfs::Origin::TestFixture)?;
    }
    workflow::build_test_initramfs_with_users(
        profile,
        Some(root),
        Some(init),
        Some(output),
        users,
        &provenance,
    )
}

/// Build a userspace API contract image from exact production ELF artifacts.
///
/// # Arguments
///
/// * `profile` - Artifact profile used by test-only programs.
/// * `output` - Destination CPIO path.
/// * `users` - Previously built production userspace artifacts.
///
/// # Returns
///
/// Returns the verified test initramfs path.
///
/// # Errors
///
/// Returns an error for test fixture, compile, staging, or archive failures.
pub(crate) fn prepare_userspace_contract_initramfs(
    profile: Profile,
    output: PathBuf,
    users: &workflow::ProductionUserArtifacts,
) -> Result<PathBuf> {
    prepare_test_initramfs(profile, &cases::InitImage::UserspaceContract, output, users)
}

/// Build a shell/UART contract image from the exact production shell ELF.
///
/// # Arguments
///
/// * `profile` - Artifact profile used by test-only programs.
/// * `output` - Destination CPIO path.
/// * `users` - Previously built production userspace artifacts.
///
/// # Returns
///
/// Returns the verified test initramfs path.
///
/// # Errors
///
/// Returns an error for test fixture, compile, staging, or archive failures.
pub(crate) fn prepare_shell_contract_initramfs(
    profile: Profile,
    output: PathBuf,
    users: &workflow::ProductionUserArtifacts,
) -> Result<PathBuf> {
    prepare_test_initramfs(profile, &cases::InitImage::ShellContract, output, users)
}

/// Verify that every dynamic product role is asserted by its declarative case.
///
/// # Arguments
///
/// * `users` - Product-manifest programs with contract roles and case IDs.
///
/// # Returns
///
/// Returns success when each non-structural program has a matching ordered
/// case assertion in the corresponding QEMU case file.
///
/// # Errors
///
/// Returns an error for malformed case files, unknown roles, or missing
/// `CASE_START`/`PASS` coverage.
pub(crate) fn verify_product_contract_coverage(
    users: &workflow::ProductionUserArtifacts,
) -> Result<()> {
    let cases = cases::load_all(PathBuf::from("tests/qemu/cases").as_path())?;
    let invocation_plan = crate::product_contract::Plan::load()?;
    for program in users.programs() {
        if program.contract() == "structural" {
            continue;
        }
        let case_name = format!("{}-contract", program.contract());
        let case = cases
            .iter()
            .find(|case| case.name == case_name)
            .ok_or_else(|| anyhow::anyhow!("missing product contract case {case_name}"))?;
        let invocation = invocation_plan.for_program(program.name()).ok_or_else(|| {
            anyhow::anyhow!("missing invocation for dynamic program {}", program.name())
        })?;
        if invocation.contract() != program.contract()
            || invocation.case() != program.contract_case().unwrap_or_default()
            || invocation.path() != format!("/{}", program.contract_install())
        {
            bail!("invocation plan does not match product {}", program.name());
        }
        let subject = invocation.case();
        if !steps_cover_case(&case.steps, subject) {
            bail!(
                "product program {} is not covered by {} in {}",
                program.name(),
                subject,
                case_name
            );
        }
    }
    Ok(())
}

fn steps_cover_case(steps: &[cases::Step], subject: &str) -> bool {
    if steps.iter().any(|step| {
        matches!(
            step,
            cases::Step::ExpectCase {
                subject: candidate,
                ..
            } if candidate == subject
        )
    }) {
        return true;
    }
    let started = steps.iter().any(|step| {
        matches!(
            step,
            cases::Step::Expect {
                event,
                subject: candidate,
                ..
            } if event == "CASE_START" && candidate == subject
        )
    });
    let passed = steps.iter().any(|step| {
        matches!(
            step,
            cases::Step::Expect {
                event,
                subject: candidate,
                ..
            } if event == "PASS" && candidate == subject
        )
    });
    started && passed
}

fn copy_tree(source: &std::path::Path, target: &std::path::Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&target_path)?;
            copy_tree(&source_path, &target_path)?;
        } else {
            fs::copy(source_path, target_path)?;
        }
    }
    Ok(())
}

fn parse_log_level(value: &str) -> Result<LogLevel> {
    match value {
        "error" => Ok(LogLevel::Error),
        "warn" => Ok(LogLevel::Warn),
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        "trace" => Ok(LogLevel::Trace),
        _ => bail!("invalid QEMU case log level {value:?}"),
    }
}
