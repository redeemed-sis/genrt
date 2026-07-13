use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const PRODUCT_MANIFEST: &str = "user/c/programs.toml";
const PRODUCT_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// One executable declared by the userspace product manifest.
pub(crate) struct Program {
    name: String,
    source: String,
    install: String,
    contract: String,
    contract_install: Option<String>,
    contract_case: Option<String>,
    #[serde(skip)]
    source_path: PathBuf,
}

impl Program {
    /// Return the stable build name without an extension.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Return the canonical C source path below the repository `user/c` tree.
    ///
    /// # Returns
    ///
    /// Returns the path resolved during manifest validation. The path is a
    /// regular file and cannot escape `user/c` through absolute components,
    /// parent traversal, or symlinks.
    pub(crate) fn source(&self) -> &Path {
        &self.source_path
    }

    /// Return the canonical release initramfs install path.
    pub(crate) fn install(&self) -> &str {
        &self.install
    }

    /// Return the dynamic contract role or `structural`.
    pub(crate) fn contract(&self) -> &str {
        &self.contract
    }

    /// Return the path used when staging the program in its contract image.
    pub(crate) fn contract_install(&self) -> &str {
        self.contract_install.as_deref().unwrap_or(&self.install)
    }

    /// Return the protocol case proving dynamic coverage, when required.
    pub(crate) fn contract_case(&self) -> Option<&str> {
        self.contract_case.as_deref()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductManifest {
    schema: u32,
    program: Vec<Program>,
}

/// Load and validate the repository userspace product manifest.
///
/// # Returns
///
/// Returns the ordered program declarations used for build, staging, and
/// release coverage.
///
/// # Errors
///
/// Returns an error for I/O or TOML failures, unsupported schema/roles,
/// duplicate names/install paths/dynamic contract cases, missing sources, or a
/// missing unique init.
pub(crate) fn load() -> Result<Vec<Program>> {
    let manifest_path = repository_root().join(PRODUCT_MANIFEST);
    let source = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let mut manifest: ProductManifest =
        toml::from_str(&source).with_context(|| format!("failed to parse {PRODUCT_MANIFEST}"))?;
    if manifest.schema != PRODUCT_SCHEMA {
        bail!("unsupported product manifest schema {}", manifest.schema);
    }
    validate_programs(&mut manifest.program)?;
    Ok(manifest.program)
}

fn validate_programs(programs: &mut [Program]) -> Result<()> {
    if programs.is_empty() {
        bail!("product manifest has no programs");
    }

    let mut names = HashSet::new();
    let mut installs = HashSet::new();
    let mut dynamic_contract_cases = HashSet::new();
    let mut init_count = 0usize;
    for program in programs {
        if !valid_identifier(program.name()) || !names.insert(program.name.clone()) {
            bail!(
                "invalid or duplicate product program name {:?}",
                program.name
            );
        }
        validate_archive_path(program.install())?;
        validate_archive_path(program.contract_install())?;
        if !installs.insert(program.install.clone()) {
            bail!("duplicate product install path {:?}", program.install);
        }
        if program.install == "init" {
            init_count += 1;
        }
        if !matches!(program.contract(), "shell" | "userspace" | "structural") {
            bail!(
                "unsupported contract role {:?} for {}",
                program.contract,
                program.name
            );
        }
        match (program.contract(), program.contract_case()) {
            ("structural", None) => {}
            ("structural", Some(_)) => {
                bail!(
                    "structural program {} must not declare contract_case",
                    program.name
                )
            }
            (_, Some(case)) if valid_identifier(case) => {
                if !dynamic_contract_cases.insert((program.contract.clone(), case.to_owned())) {
                    bail!(
                        "duplicate dynamic contract case ({:?}, {:?})",
                        program.contract,
                        case
                    );
                }
            }
            _ => bail!("program {} requires a valid contract_case", program.name),
        }
        program.source_path = validate_product_source(&program.source)?;
    }
    if init_count != 1 {
        bail!("product manifest must install exactly one program as init");
    }
    Ok(())
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .to_path_buf()
}

fn validate_product_source(source: &str) -> Result<PathBuf> {
    let root = repository_root().canonicalize()?;
    let allowed_root = root.join("user/c").canonicalize()?;
    validate_product_source_below(source, &root, &allowed_root)
}

fn validate_product_source_below(
    source: &str,
    root: &Path,
    allowed_root: &Path,
) -> Result<PathBuf> {
    let relative = Path::new(source);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("product source must be a canonical repository-relative path: {source:?}");
    }

    let resolved = root
        .join(relative)
        .canonicalize()
        .with_context(|| format!("failed to resolve product source {source:?}"))?;
    if !resolved.starts_with(allowed_root) || !resolved.is_file() {
        bail!("product source must be a regular file below user/c: {source:?}");
    }
    Ok(resolved)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_archive_path(path: &str) -> Result<()> {
    if path.is_empty() || path.starts_with('/') || path.ends_with('/') {
        bail!("non-canonical product install path {path:?}");
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        bail!("non-canonical product install path {path:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[cfg(unix)]
    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn repository_product_manifest_is_valid() {
        let programs = load().unwrap();
        assert_eq!(
            programs
                .iter()
                .filter(|program| program.install() == "init")
                .count(),
            1
        );
    }

    #[test]
    fn rejects_duplicate_dynamic_contract_case() {
        let mut programs = load().unwrap();
        let original = programs
            .iter()
            .find(|program| program.contract() != "structural")
            .unwrap()
            .clone();
        programs.push(Program {
            name: "duplicate-contract-program".to_owned(),
            install: "bin/duplicate-contract-program".to_owned(),
            ..original
        });

        assert!(validate_programs(&mut programs).is_err());
    }

    #[test]
    fn rejects_unknown_manifest_fields() {
        let source = r#"
            schema = 1
            unexpected = true
            program = []
        "#;
        assert!(toml::from_str::<ProductManifest>(source).is_err());
    }

    #[test]
    fn rejects_product_sources_outside_user_tree() {
        let mut programs = load().unwrap();
        for source in [
            "tests/qemu/user/api_case.c",
            "../outside.c",
            "/tmp/outside.c",
        ] {
            programs[0].source = source.to_owned();
            assert!(
                validate_programs(&mut programs).is_err(),
                "accepted {source}"
            );
            programs = load().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_product_source_symlink_escape() {
        use std::os::unix::fs::symlink;

        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("genrt-product-source-{}-{id}", std::process::id()));
        let allowed = root.join("user/c");
        fs::create_dir_all(&allowed).unwrap();
        fs::write(root.join("outside.c"), b"int main(void) { return 0; }").unwrap();
        symlink(root.join("outside.c"), allowed.join("escape.c")).unwrap();

        assert!(validate_product_source_below("user/c/escape.c", &root, &allowed).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
