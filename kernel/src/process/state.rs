#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum ProcessState {
    Free,
    Running,
    Zombie,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessExitStatus {
    Exited(usize),
    Faulted(UserFault),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UserFault {
    pub kind: UserFaultKind,
}

impl UserFault {
    pub const fn unknown_syscall(nr: usize) -> Self {
        Self {
            kind: UserFaultKind::UnknownSyscall(nr),
        }
    }

    pub const fn sync_exception(kind: UserFaultKind) -> Self {
        Self { kind }
    }
}

/// Stable classification of a fault attributed to a lower-EL process.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UserFaultKind {
    /// Userspace requested a syscall number not implemented by the kernel.
    UnknownSyscall(usize),
    /// Non-translation/non-permission instruction abort.
    InstructionAbort,
    /// Non-translation/non-permission data abort.
    DataAbort,
    /// Instruction fetch failed address translation.
    InstructionTranslationFault,
    /// Data access failed address translation.
    DataTranslationFault,
    /// Instruction fetch violated page permissions.
    InstructionPermissionFault,
    /// Data access violated page permissions.
    DataPermissionFault,
    /// Other lower-EL synchronous exception.
    OtherSync,
}
