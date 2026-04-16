use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "Engineering workflow helper for genrt")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Doctor,
    Phase0Check,
    RepoTree,
    QemuCmd {
        #[arg(long, value_enum)]
        arch: Arch,
    },
    GdbCmd {
        #[arg(long, value_enum)]
        arch: Arch,
    },
    BuildAarch64 {
        #[arg(long, value_enum)]
        log_level: Option<LogLevel>,
    },
    RunAarch64 {
        #[arg(long, value_enum)]
        log_level: Option<LogLevel>,
    },
    DebugAarch64 {
        #[arg(long, value_enum)]
        log_level: Option<LogLevel>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Arch {
    Aarch64,
    X8664,
    Riscv64,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn feature_name(self) -> &'static str {
        match self {
            Self::Error => "log-level-error",
            Self::Warn => "log-level-warn",
            Self::Info => "log-level-info",
            Self::Debug => "log-level-debug",
            Self::Trace => "log-level-trace",
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Doctor => doctor(),
        Commands::Phase0Check => phase0_check(),
        Commands::RepoTree => repo_tree(),
        Commands::QemuCmd { arch } => qemu_cmd(arch),
        Commands::GdbCmd { arch } => gdb_cmd(arch),
        Commands::BuildAarch64 { log_level } => build_aarch64(log_level),
        Commands::RunAarch64 { log_level } => run_aarch64(false, log_level),
        Commands::DebugAarch64 { log_level } => {
            run_aarch64(true, Some(log_level.unwrap_or(LogLevel::Debug)))
        }
    }
}

fn doctor() -> Result<()> {
    let checks = [
        "cargo",
        "rustup",
        "just",
        "qemu-system-aarch64",
        "gdb",
        "aarch64-linux-gnu-gdb",
        "ld.lld",
    ];

    println!("== tool availability ==");
    let mut missing = Vec::new();

    for tool in checks {
        match which(tool) {
            Some(path) => println!("[ok] {tool:<20} {path}"),
            None => {
                println!("[missing] {tool}");
                missing.push(tool);
            }
        }
    }

    println!("\n== rustup targets ==");
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .context("failed to query rustup targets")?;
    let installed = String::from_utf8_lossy(&output.stdout);
    for target in [
        "aarch64-unknown-none",
        "x86_64-unknown-none",
        "riscv64gc-unknown-none-elf",
    ] {
        let status = if installed.lines().any(|line| line.trim() == target) {
            "ok"
        } else {
            "missing"
        };
        println!("[{status}] {target}");
    }

    if missing.is_empty() {
        Ok(())
    } else {
        bail!("some required tools are missing")
    }
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

fn qemu_cmd(arch: Arch) -> Result<()> {
    match arch {
        Arch::Aarch64 => {
            println!("qemu-system-aarch64 \\");
            println!("  -machine virt \\");
            println!("  -cpu cortex-a72 \\");
            println!("  -nographic \\");
            println!("  -serial mon:stdio \\");
            println!("  -kernel target/aarch64-unknown-none/debug/genrt-aarch64.elf \\");
            println!("  -S -s");
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
            println!("aarch64-linux-gnu-gdb target/aarch64-unknown-none/debug/genrt-aarch64.elf");
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

fn build_aarch64(log_level: Option<LogLevel>) -> Result<()> {
    let mut build = Command::new("cargo");
    build.args([
        "build",
        "-p",
        "genrt-arch-aarch64",
        "--target",
        "aarch64-unknown-none",
    ]);

    if let Some(log_level) = log_level {
        build.args(["-p", "kernel", "--features", log_level.feature_name()]);
    }

    let status = build.status().context("failed to invoke cargo build")?;

    if !status.success() {
        bail!("cargo build failed for genrt-arch-aarch64")
    }

    let final_elf = final_elf_path();
    if let Some(parent) = final_elf.parent() {
        fs::create_dir_all(parent).context("failed to create output directory for final elf")?;
    }

    let staticlib = locate_staticlib()?;
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

    println!("built {}", final_elf.display());
    Ok(())
}

fn run_aarch64(wait_for_gdb: bool, log_level: Option<LogLevel>) -> Result<()> {
    build_aarch64(log_level)?;

    let mut cmd = Command::new("qemu-system-aarch64");
    cmd.args([
        "-machine",
        "virt",
        "-cpu",
        "cortex-a72",
        "-nographic",
        "-serial",
        "mon:stdio",
        "-kernel",
    ])
    .arg(final_elf_path())
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit());

    if wait_for_gdb {
        cmd.args(["-S", "-s"]);
    }

    let status = cmd
        .status()
        .context("failed to invoke qemu-system-aarch64")?;
    if status.success() {
        Ok(())
    } else {
        bail!("QEMU exited with a non-zero status")
    }
}

fn final_elf_path() -> PathBuf {
    PathBuf::from("target/aarch64-unknown-none/debug/genrt-aarch64.elf")
}

fn locate_staticlib() -> Result<PathBuf> {
    let direct = PathBuf::from("target/aarch64-unknown-none/debug/libgenrt_arch_aarch64.a");
    if direct.exists() {
        return Ok(direct);
    }

    let deps = Path::new("target/aarch64-unknown-none/debug/deps");
    if deps.is_dir() {
        let mut candidates = Vec::new();
        for entry in fs::read_dir(deps).context("failed to scan target deps directory")? {
            let entry = entry.context("failed to read target deps entry")?;
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.starts_with("libgenrt_arch_aarch64") && name.ends_with(".a") {
                    candidates.push(path);
                }
            }
        }
        candidates.sort();
        if let Some(last) = candidates.pop() {
            return Ok(last);
        }
    }

    bail!("unable to locate libgenrt_arch_aarch64.a after cargo build")
}

fn ensure_exists(path: &str) -> Result<()> {
    if Path::new(path).exists() {
        Ok(())
    } else {
        bail!("missing required path: {path}")
    }
}

fn which(tool: &str) -> Option<String> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.display().to_string())
}
