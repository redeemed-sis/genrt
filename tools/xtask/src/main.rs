use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

const AARCH64_TARGET: &str = "aarch64-unknown-none-softfloat";
const AARCH64_QEMU_MACHINE: &str = "virt,gic-version=2";
const AARCH64_QEMU_CPU: &str = "cortex-a72";
const AARCH64_DTB_LOAD_ADDR: &str = "0x40000000";
const AARCH64_USER_IMAGE_LOAD_ADDR: &str = "0x47000000";
const AARCH64_USER_IMAGE_MAX_SIZE: u64 = 4096;

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
        #[arg(long)]
        user_bin: Option<PathBuf>,
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
        #[arg(long)]
        user_bin: Option<PathBuf>,
    },
    DebugAarch64 {
        #[arg(long, value_enum)]
        log_level: Option<LogLevel>,
        #[arg(long)]
        user_bin: Option<PathBuf>,
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
        Commands::QemuCmd { arch, user_bin } => qemu_cmd(arch, user_bin),
        Commands::GdbCmd { arch } => gdb_cmd(arch),
        Commands::BuildAarch64 { log_level } => build_aarch64(log_level),
        Commands::RunAarch64 {
            log_level,
            user_bin,
        } => run_aarch64(false, log_level, user_bin),
        Commands::DebugAarch64 {
            log_level,
            user_bin,
        } => run_aarch64(true, Some(log_level.unwrap_or(LogLevel::Debug)), user_bin),
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
        AARCH64_TARGET,
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

fn qemu_cmd(arch: Arch, user_bin: Option<PathBuf>) -> Result<()> {
    match arch {
        Arch::Aarch64 => {
            let user_bin = user_bin.unwrap_or_else(default_user_bin_path);
            println!("qemu-system-aarch64 \\");
            println!("  -machine {AARCH64_QEMU_MACHINE} \\");
            println!("  -cpu {AARCH64_QEMU_CPU} \\");
            println!("  -nographic \\");
            println!("  -serial mon:stdio \\");
            println!("  -kernel target/{AARCH64_TARGET}/debug/genrt-aarch64.elf \\");
            println!(
                "  -device loader,file=target/{AARCH64_TARGET}/debug/qemu-virt.dtb,addr={AARCH64_DTB_LOAD_ADDR} \\"
            );
            println!(
                "  -device loader,file={},addr={AARCH64_USER_IMAGE_LOAD_ADDR},force-raw=on \\",
                user_bin.display()
            );
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

fn build_aarch64(log_level: Option<LogLevel>) -> Result<()> {
    let dtb_path = generate_qemu_virt_dtb()?;
    build_aarch64_user_hello()?;

    let mut build = Command::new("cargo");
    build.args([
        "build",
        "-p",
        "genrt-arch-aarch64",
        "--target",
        AARCH64_TARGET,
    ]);
    build.env("GENRT_AARCH64_DTB_PATH", &dtb_path);

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

    verify_aarch64_boot_text_autonomy(&final_elf)?;
    println!("built {}", final_elf.display());
    Ok(())
}

fn run_aarch64(
    wait_for_gdb: bool,
    log_level: Option<LogLevel>,
    user_bin: Option<PathBuf>,
) -> Result<()> {
    build_aarch64(log_level)?;
    let dtb_path = qemu_virt_dtb_path()?;
    let user_bin = user_bin.unwrap_or_else(default_user_bin_path);

    let mut cmd = Command::new("qemu-system-aarch64");
    cmd.args([
        "-machine",
        AARCH64_QEMU_MACHINE,
        "-cpu",
        AARCH64_QEMU_CPU,
        "-nographic",
        "-serial",
        "mon:stdio",
        "-kernel",
    ])
    .arg(final_elf_path())
    .arg("-device")
    .arg(format!(
        "loader,file={},addr={AARCH64_DTB_LOAD_ADDR}",
        dtb_path.display()
    ))
    .arg("-device")
    .arg(format!(
        "loader,file={},addr={AARCH64_USER_IMAGE_LOAD_ADDR},force-raw=on",
        user_bin.display()
    ))
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
    PathBuf::from(format!("target/{AARCH64_TARGET}/debug/genrt-aarch64.elf"))
}

fn default_user_bin_path() -> PathBuf {
    PathBuf::from(format!("target/{AARCH64_TARGET}/debug/user/hello.bin"))
}

fn build_aarch64_user_hello() -> Result<PathBuf> {
    let source = Path::new("user/aarch64/hello.S");
    ensure_exists(source.to_str().unwrap_or("user/aarch64/hello.S"))?;

    let bin = default_user_bin_path();
    let obj = bin.with_extension("o");
    if let Some(parent) = bin.parent() {
        fs::create_dir_all(parent).context("failed to create user binary output directory")?;
    }

    let status = Command::new("llvm-mc")
        .args(["-triple=aarch64-none-elf", "-filetype=obj"])
        .arg(source)
        .arg("-o")
        .arg(&obj)
        .status()
        .context("failed to invoke llvm-mc for AArch64 user program")?;
    if !status.success() {
        bail!("llvm-mc failed to assemble AArch64 user program")
    }

    let status = Command::new("llvm-objcopy")
        .args(["-O", "binary"])
        .arg(&obj)
        .arg(&bin)
        .status()
        .context("failed to invoke llvm-objcopy for AArch64 user program")?;
    if !status.success() {
        bail!("llvm-objcopy failed to produce AArch64 user binary")
    }

    let size = fs::metadata(&bin)
        .with_context(|| format!("failed to stat {}", bin.display()))?
        .len();
    if size == 0 || size > AARCH64_USER_IMAGE_MAX_SIZE {
        bail!(
            "AArch64 user binary size {size} exceeds bring-up mapping limit {AARCH64_USER_IMAGE_MAX_SIZE}"
        );
    }

    println!("built {} ({} bytes)", bin.display(), size);
    Ok(bin)
}

fn qemu_virt_dtb_path() -> Result<PathBuf> {
    Ok(env::current_dir()
        .context("failed to query current directory")?
        .join(format!("target/{AARCH64_TARGET}/debug/qemu-virt.dtb")))
}

fn generate_qemu_virt_dtb() -> Result<PathBuf> {
    let dtb_path = qemu_virt_dtb_path()?;
    if let Some(parent) = dtb_path.parent() {
        fs::create_dir_all(parent).context("failed to create DTB output directory")?;
    }

    let status = Command::new("qemu-system-aarch64")
        .arg("-machine")
        .arg(format!(
            "{AARCH64_QEMU_MACHINE},dumpdtb={}",
            dtb_path.display()
        ))
        .args([
            "-cpu",
            AARCH64_QEMU_CPU,
            "-display",
            "none",
            "-serial",
            "null",
            "-monitor",
            "none",
        ])
        .status()
        .context("failed to invoke qemu-system-aarch64 for DTB generation")?;

    if !status.success() {
        bail!("qemu-system-aarch64 failed to generate virt DTB")
    }

    compact_dtb(&dtb_path)?;
    trim_dtb_to_fdt_totalsize(&dtb_path)?;
    Ok(dtb_path)
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

fn locate_staticlib() -> Result<PathBuf> {
    let direct = PathBuf::from(format!(
        "target/{AARCH64_TARGET}/debug/libgenrt_arch_aarch64.a"
    ));
    if direct.exists() {
        return Ok(direct);
    }

    let deps_buf = PathBuf::from(format!("target/{AARCH64_TARGET}/debug/deps"));
    let deps = deps_buf.as_path();
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
