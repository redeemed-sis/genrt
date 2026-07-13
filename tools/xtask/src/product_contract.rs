use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::product::{self, Program};

const CONTRACT_MANIFEST: &str = "tests/qemu/program-contracts.toml";
const CONTRACT_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestInvocation {
    program: String,
    case: String,
    path: String,
    args: Vec<String>,
    expected_exit: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractManifest {
    schema: u32,
    invocation: Vec<ManifestInvocation>,
}

/// One validated execution that dynamically covers a production program.
#[derive(Clone, Debug)]
pub(crate) struct Invocation {
    program: String,
    contract: String,
    case: String,
    path: String,
    args: Vec<String>,
    expected_exit: u8,
}

impl Invocation {
    /// Return the product program name covered by this invocation.
    ///
    /// # Returns
    ///
    /// Returns the exact program key from the invocation manifest.
    pub(crate) fn program(&self) -> &str {
        &self.program
    }

    /// Return the contract role whose supervisor executes this invocation.
    ///
    /// # Returns
    ///
    /// Returns the role inherited from the matching product declaration.
    pub(crate) fn contract(&self) -> &str {
        &self.contract
    }

    /// Return the protocol case emitted around this invocation.
    ///
    /// # Returns
    ///
    /// Returns the validated unique case identifier.
    pub(crate) fn case(&self) -> &str {
        &self.case
    }

    /// Return the exact guest executable path passed to `execve`.
    ///
    /// # Returns
    ///
    /// Returns the absolute guest path matching the product contract install
    /// path.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// Return the expected child exit code.
    ///
    /// # Returns
    ///
    /// Returns the bounded status compared after `waitpid`.
    pub(crate) const fn expected_exit(&self) -> u8 {
        self.expected_exit
    }
}

/// Validated one-to-one dynamic invocation plan for production programs.
pub(crate) struct Plan {
    invocations: Vec<Invocation>,
}

impl Plan {
    /// Load and validate the repository product invocation plan.
    ///
    /// # Returns
    ///
    /// Returns a plan in manifest order. Every non-structural product has
    /// exactly one invocation tied to its declared contract role, case, and
    /// contract install path.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O/TOML failures, unknown fields or programs,
    /// duplicate/missing invocations, mismatched cases or paths, and invalid
    /// bounded arguments.
    pub(crate) fn load() -> Result<Self> {
        let source = fs::read_to_string(contract_manifest_path())
            .with_context(|| format!("failed to read {CONTRACT_MANIFEST}"))?;
        let manifest: ContractManifest = toml::from_str(&source)
            .with_context(|| format!("failed to parse {CONTRACT_MANIFEST}"))?;
        if manifest.schema != CONTRACT_SCHEMA {
            bail!("unsupported program contract schema {}", manifest.schema);
        }
        validate(product::load()?, manifest.invocation)
    }

    /// Find the invocation that dynamically covers one product program.
    ///
    /// # Arguments
    ///
    /// * `program` - Product name from `user/c/programs.toml`.
    ///
    /// # Returns
    ///
    /// Returns the unique invocation, or `None` for structural-only or unknown
    /// programs.
    pub(crate) fn for_program(&self, program: &str) -> Option<&Invocation> {
        self.invocations
            .iter()
            .find(|invocation| invocation.program() == program)
    }

    /// Generate the C invocation table consumed by one test supervisor.
    ///
    /// # Arguments
    ///
    /// * `output_dir` - Directory that receives `product_contracts.h`.
    /// * `contract` - Dynamic role selected for the supervisor.
    ///
    /// # Returns
    ///
    /// Returns the generated header path.
    ///
    /// # Errors
    ///
    /// Returns an error when the role has no invocation or the output cannot
    /// be created or written.
    pub(crate) fn write_c_header(&self, output_dir: &Path, contract: &str) -> Result<PathBuf> {
        let selected = self
            .invocations
            .iter()
            .filter(|invocation| invocation.contract == contract)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            bail!("program contract role {contract:?} has no invocations");
        }

        fs::create_dir_all(output_dir)?;
        let mut header = String::from("#pragma once\n\nstruct gtrt_program_contract {\n");
        header.push_str("    const char *case_name;\n    const char *path;\n");
        header.push_str("    char *const *argv;\n    int expected_exit;\n};\n\n");
        for (index, invocation) in selected.iter().enumerate() {
            header.push_str(&format!("static char *gtrt_contract_{index}_argv[] = {{"));
            for arg in &invocation.args {
                header.push_str(&format!("\"{}\", ", escape_c_string(arg)));
            }
            header.push_str("NULL};\n");
        }
        header
            .push_str("\nstatic const struct gtrt_program_contract GTRT_PROGRAM_CONTRACTS[] = {\n");
        for (index, invocation) in selected.iter().enumerate() {
            header.push_str(&format!(
                "    {{\"{}\", \"{}\", gtrt_contract_{index}_argv, {}}},\n",
                escape_c_string(&invocation.case),
                escape_c_string(&invocation.path),
                invocation.expected_exit
            ));
        }
        header.push_str("};\n#define GTRT_PROGRAM_CONTRACT_COUNT ");
        header.push_str(&selected.len().to_string());
        header.push('\n');

        let path = output_dir.join("product_contracts.h");
        fs::write(&path, header)?;
        Ok(path)
    }
}

fn validate(programs: Vec<Program>, entries: Vec<ManifestInvocation>) -> Result<Plan> {
    let mut program_names = HashSet::new();
    let mut cases = HashSet::new();
    let mut invocations = Vec::new();
    invocations.try_reserve_exact(entries.len())?;

    for entry in entries {
        let program = programs
            .iter()
            .find(|program| program.name() == entry.program)
            .ok_or_else(|| anyhow::anyhow!("contract names unknown program {:?}", entry.program))?;
        if program.contract() == "structural" {
            bail!(
                "structural program {} must not have an invocation",
                program.name()
            );
        }
        if !program_names.insert(entry.program.clone()) {
            bail!("duplicate invocation for program {}", entry.program);
        }
        if !cases.insert(entry.case.clone()) {
            bail!("duplicate program contract case {}", entry.case);
        }
        if program.contract_case() != Some(entry.case.as_str()) {
            bail!("contract case mismatch for program {}", program.name());
        }
        let expected_path = format!("/{}", program.contract_install());
        if entry.path != expected_path {
            bail!(
                "contract path mismatch for {}: expected {expected_path:?}, got {:?}",
                program.name(),
                entry.path
            );
        }
        if entry.args.is_empty() || entry.args.len() > 16 {
            bail!(
                "contract argv for {} must contain 1..=16 entries",
                program.name()
            );
        }
        if entry.args.iter().any(|arg| {
            arg.len() > 256
                || arg
                    .bytes()
                    .any(|byte| !(byte.is_ascii_graphic() || byte == b' '))
        }) {
            bail!(
                "contract argv for {} contains invalid bytes",
                program.name()
            );
        }
        invocations.push(Invocation {
            program: entry.program,
            contract: program.contract().to_owned(),
            case: entry.case,
            path: entry.path,
            args: entry.args,
            expected_exit: entry.expected_exit,
        });
    }

    for program in &programs {
        let present = program_names.contains(program.name());
        if program.contract() == "structural" {
            if present {
                bail!(
                    "structural program {} has a dynamic invocation",
                    program.name()
                );
            }
        } else if !present {
            bail!("dynamic program {} has no invocation", program.name());
        }
    }
    if invocations
        .iter()
        .filter(|invocation| invocation.contract == "shell")
        .count()
        != 1
    {
        bail!("shell contract requires exactly one production shell invocation");
    }
    Ok(Plan { invocations })
}

fn escape_c_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn contract_manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(CONTRACT_MANIFEST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_invocation_plan_is_one_to_one() {
        let plan = Plan::load().unwrap();
        let programs = product::load().unwrap();
        assert_eq!(
            plan.invocations.len(),
            programs
                .iter()
                .filter(|program| program.contract() != "structural")
                .count()
        );
    }

    #[test]
    fn rejects_case_reused_for_wrong_executable() {
        let programs = product::load().unwrap();
        let source = fs::read_to_string(contract_manifest_path()).unwrap();
        let mut manifest: ContractManifest = toml::from_str(&source).unwrap();
        manifest.invocation[1].case = "file-io".to_owned();
        assert!(validate(programs, manifest.invocation).is_err());
    }

    #[test]
    fn rejects_contract_path_mismatch() {
        let programs = product::load().unwrap();
        let source = fs::read_to_string(contract_manifest_path()).unwrap();
        let mut manifest: ContractManifest = toml::from_str(&source).unwrap();
        manifest.invocation[1].path = "/bin/not-echo".to_owned();
        assert!(validate(programs, manifest.invocation).is_err());
    }
}
