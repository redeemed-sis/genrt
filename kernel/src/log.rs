use core::fmt::{self, Write};

use crate::console;

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    #[inline(always)]
    const fn tag(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }
}

macro_rules! ensure_unique_log_level_feature {
    ($selected:literal; $($other:literal),+ $(,)?) => {
        #[cfg(all(feature = $selected, any($(feature = $other),+)))]
        compile_error!("enable at most one kernel log-level-* feature at a time");
    };
}

ensure_unique_log_level_feature!(
    "log-level-error";
    "log-level-warn",
    "log-level-info",
    "log-level-debug",
    "log-level-trace"
);
ensure_unique_log_level_feature!(
    "log-level-warn";
    "log-level-info",
    "log-level-debug",
    "log-level-trace"
);
ensure_unique_log_level_feature!(
    "log-level-info";
    "log-level-debug",
    "log-level-trace"
);
ensure_unique_log_level_feature!("log-level-debug"; "log-level-trace");

pub const ACTIVE_LOG_LEVEL: LogLevel = if cfg!(feature = "log-level-error") {
    LogLevel::Error
} else if cfg!(feature = "log-level-warn") {
    LogLevel::Warn
} else if cfg!(feature = "log-level-info") {
    LogLevel::Info
} else if cfg!(feature = "log-level-debug") {
    LogLevel::Debug
} else if cfg!(feature = "log-level-trace") {
    LogLevel::Trace
} else {
    LogLevel::Info
};

struct ConsoleWriter;

impl Write for ConsoleWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        console::puts(s);
        Ok(())
    }
}

#[inline(always)]
pub fn enabled(level: LogLevel) -> bool {
    level <= ACTIVE_LOG_LEVEL
}

#[inline]
pub fn _print(args: fmt::Arguments<'_>) {
    let mut writer = ConsoleWriter;
    let _ = writer.write_fmt(args);
}

#[inline]
pub fn _log(level: LogLevel, args: fmt::Arguments<'_>) {
    // Logging is allocation-free and lock-free, so occasional use from IRQ context is
    // acceptable during bring-up. Trace-heavy logging can still perturb timing and should
    // remain a debug-oriented tool rather than a steady-state execution mode.
    let mut writer = ConsoleWriter;
    let _ = writer.write_str("[");
    let _ = writer.write_str(level.tag());
    let _ = writer.write_str("] ");
    let _ = writer.write_fmt(args);
    let _ = writer.write_str("\n");
}

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {{
        $crate::log::_print(core::format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! kprintln {
    () => {{
        $crate::log::_print(core::format_args!("\n"));
    }};
    ($($arg:tt)*) => {{
        $crate::log::_print(core::format_args!($($arg)*));
        $crate::log::_print(core::format_args!("\n"));
    }};
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {{
        if $crate::log::enabled($crate::log::LogLevel::Error) {
            $crate::log::_log($crate::log::LogLevel::Error, core::format_args!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {{
        if $crate::log::enabled($crate::log::LogLevel::Warn) {
            $crate::log::_log($crate::log::LogLevel::Warn, core::format_args!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {{
        if $crate::log::enabled($crate::log::LogLevel::Info) {
            $crate::log::_log($crate::log::LogLevel::Info, core::format_args!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {{
        if $crate::log::enabled($crate::log::LogLevel::Debug) {
            $crate::log::_log($crate::log::LogLevel::Debug, core::format_args!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {{
        if $crate::log::enabled($crate::log::LogLevel::Trace) {
            $crate::log::_log($crate::log::LogLevel::Trace, core::format_args!($($arg)*));
        }
    }};
}
