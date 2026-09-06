use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::artifacts::Profile;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum InitImage {
    KernelContract,
    UserFault,
    UserspaceContract,
    SmpUserspaceContract,
    ShellContract,
    RebootContract,
    PoweroffContract,
    SmpRebootContract,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Termination {
    /// Require the supervisor's terminal `DONE ... PASS` record.
    #[default]
    Protocol,
    /// Require QEMU to exit successfully after a PSCI system-off request.
    Poweroff,
    /// Require a fresh supervisor `READY` epoch after a PSCI reset request.
    Reboot,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Case {
    pub(super) name: String,
    pub(super) suite: String,
    pub(super) supervisor: String,
    #[serde(default = "default_profile")]
    pub(super) profile: Profile,
    #[serde(default = "default_log_level")]
    pub(super) log_level: String,
    #[serde(default)]
    pub(super) kernel_features: Vec<String>,
    #[serde(default = "default_cpu_count")]
    pub(super) cpu_count: usize,
    pub(super) init: InitImage,
    #[serde(default)]
    pub(super) termination: Termination,
    #[serde(default = "default_case_timeout")]
    pub(super) timeout_secs: u64,
    #[serde(default = "default_step_timeout")]
    pub(super) step_timeout_secs: u64,
    pub(super) steps: Vec<Step>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Step {
    SendLine {
        value: String,
    },
    Expect {
        producer: String,
        event: String,
        subject: String,
        detail: Option<String>,
    },
    ExpectCase {
        producer: String,
        subject: String,
    },
    Challenge {
        command: String,
        producer: String,
        subject: String,
    },
}

fn default_profile() -> Profile {
    Profile::Debug
}

fn default_log_level() -> String {
    "info".to_owned()
}

fn default_case_timeout() -> u64 {
    60
}

fn default_cpu_count() -> usize {
    1
}

fn default_step_timeout() -> u64 {
    15
}

pub(super) fn load_all(root: &Path) -> Result<Vec<Case>> {
    let mut paths = fs::read_dir(root)
        .with_context(|| format!("failed to read QEMU cases from {}", root.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to enumerate {}", root.display()))?;
    paths.retain(|path| path.extension().is_some_and(|ext| ext == "toml"));
    paths.sort();

    let mut cases = Vec::new();
    for path in paths {
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let case: Case = toml::from_str(&source)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        validate_cpu_count(&case)?;
        if cases
            .iter()
            .any(|existing: &Case| existing.name == case.name)
        {
            bail!("duplicate QEMU case name {}", case.name);
        }
        cases.push(case);
    }
    if cases.is_empty() {
        bail!("no QEMU cases found in {}", root.display());
    }
    Ok(cases)
}

fn validate_cpu_count(case: &Case) -> Result<()> {
    if !(1..=crate::qemu::MAX_CPUS).contains(&case.cpu_count) {
        bail!(
            "QEMU case {} requests {} CPUs; supported range is 1..={}",
            case.name,
            case.cpu_count,
            crate::qemu::MAX_CPUS
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_case_fields() {
        let source = r#"
            name = "case"
            suite = "suite"
            supervisor = "supervisor"
            init = "kernel-contract"
            timeout_sec = 10
            steps = []
        "#;
        assert!(toml::from_str::<Case>(source).is_err());
    }

    #[test]
    fn defaults_to_one_cpu_and_rejects_excessive_topology() {
        let source = r#"
            name = "case"
            suite = "suite"
            supervisor = "supervisor"
            init = "kernel-contract"
            steps = []
        "#;
        let mut case = toml::from_str::<Case>(source).expect("valid case");
        assert_eq!(case.cpu_count, 1);
        assert_eq!(case.termination, Termination::Protocol);
        assert!(validate_cpu_count(&case).is_ok());

        case.cpu_count = crate::qemu::MAX_CPUS + 1;
        assert!(validate_cpu_count(&case).is_err());
    }
}
