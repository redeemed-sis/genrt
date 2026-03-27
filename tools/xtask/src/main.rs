use std::{env, path::Path, process::Command};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "Engineering workflow helper for hardrt")]
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
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Arch {
    Aarch64,
    X8664,
    Riscv64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Doctor => doctor(),
        Commands::Phase0Check => phase0_check(),
        Commands::RepoTree => repo_tree(),
        Commands::QemuCmd { arch } => qemu_cmd(arch),
        Commands::GdbCmd { arch } => gdb_cmd(arch),
    }
}

fn doctor() -> Result<()> {
    let checks = [
        "cargo",
        "rustup",
        "just",
        "qemu-system-aarch64",
        "gdb",
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
    ];

    println!("== phase 0 week 1 checklist ==");
    for path in required_paths {
        ensure_exists(path)?;
        println!("[ok] {path}");
    }

    println!("\nPhase 0 / Week 1 scaffold is present.");
    Ok(())
}

fn repo_tree() -> Result<()> {
    println!("hardrt/");
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
            println!("aarch64-linux-gnu-gdb");
            println!("(gdb) file target/aarch64-unknown-none/debug/kernel");
            println!("(gdb) target remote :1234");
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
