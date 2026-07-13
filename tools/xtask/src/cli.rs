use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::artifacts::Profile;

/// Parsed xtask command line.
#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "Engineering workflow helper for genrt")]
pub(crate) struct Cli {
    /// Selected workflow and command-specific arguments.
    #[command(subcommand)]
    pub(crate) command: Commands,
}

/// Repository workflow commands exposed by xtask.
#[derive(Subcommand)]
pub(crate) enum Commands {
    Doctor,
    Check,
    TestAarch64 {
        #[arg(long)]
        case: Option<String>,
        #[arg(long)]
        list: bool,
        #[arg(long, value_enum)]
        profile: Option<Profile>,
        #[arg(long, default_value_t = 60)]
        timeout_secs: u64,
        #[arg(long, default_value = "target/test-results")]
        artifacts_dir: PathBuf,
        #[arg(long)]
        keep_going: bool,
    },
    Ci,
    Dist {
        #[arg(long)]
        tag: String,
        #[arg(long, default_value = "dist")]
        output_dir: PathBuf,
    },
    RepoTree,
    QemuCmd {
        #[arg(long, value_enum)]
        arch: Arch,
        #[arg(long)]
        initramfs: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = Profile::Debug)]
        profile: Profile,
    },
    GdbCmd {
        #[arg(long, value_enum)]
        arch: Arch,
    },
    BuildAarch64 {
        #[arg(long, value_enum)]
        log_level: Option<LogLevel>,
        #[arg(long, value_enum, default_value_t = Profile::Debug)]
        profile: Profile,
    },
    BuildUserHello,
    BuildUserFault,
    BuildUserReadFile,
    BuildUserShell,
    BuildUserEcho,
    BuildUserCat,
    BuildUserLs,
    BuildUserPwd,
    BuildInitramfs {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        init: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = Profile::Debug)]
        profile: Profile,
    },
    RunAarch64 {
        #[arg(long, value_enum)]
        log_level: Option<LogLevel>,
        #[arg(long)]
        initramfs: Option<PathBuf>,
        #[arg(long)]
        initramfs_root: Option<PathBuf>,
        #[arg(long)]
        init: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = Profile::Debug)]
        profile: Profile,
    },
    DebugAarch64 {
        #[arg(long, value_enum)]
        log_level: Option<LogLevel>,
        #[arg(long)]
        initramfs: Option<PathBuf>,
        #[arg(long)]
        initramfs_root: Option<PathBuf>,
        #[arg(long)]
        init: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = Profile::Debug)]
        profile: Profile,
    },
}

/// Architecture selector retained by command-preview helpers.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum Arch {
    Aarch64,
    X8664,
    Riscv64,
}

/// Compile-time kernel logging threshold.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// Return the Cargo feature corresponding to this log threshold.
    ///
    /// The returned static string can be passed directly in a feature list and
    /// requires no allocation.
    pub(crate) const fn feature_name(self) -> &'static str {
        match self {
            Self::Error => "log-level-error",
            Self::Warn => "log-level-warn",
            Self::Info => "log-level-info",
            Self::Debug => "log-level-debug",
            Self::Trace => "log-level-trace",
        }
    }
}
