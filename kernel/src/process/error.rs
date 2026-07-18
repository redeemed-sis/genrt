use crate::{
    errno,
    fs::initramfs::InitramfsError,
    loader::elf::ElfLoadError,
    memory::{user::UserCopyError, vm::VmError},
    sched,
};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProcessError {
    InvalidProcess,
    NoProcessSlots,
    Vm(VmError),
    OutOfFrames,
    Spawn(sched::SpawnError),
    Elf(ElfLoadError),
    Initramfs(InitramfsError),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProcessJoinError {
    InvalidProcess,
    JoinInProgress,
    SelfJoin,
    SchedulerNotInitialized,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessFaultError {
    NoCurrentProcess,
    InvalidProcess,
}

/// Errors returned by process current-working-directory operations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProcessPathError {
    /// The caller is not running as a userspace process.
    NoCurrentProcess,
    /// The stored or requested ramfs directory index is invalid.
    InvalidDirectory,
}

pub(super) fn fork_errno(err: ProcessError) -> errno::Errno {
    match err {
        ProcessError::NoProcessSlots => errno::EAGAIN,
        ProcessError::OutOfFrames => errno::ENOMEM,
        ProcessError::Vm(err) => fork_vm_errno(err),
        ProcessError::Spawn(_) => errno::EAGAIN,
        ProcessError::InvalidProcess => errno::EINVAL,
        ProcessError::Elf(_) | ProcessError::Initramfs(_) => errno::ENOEXEC,
    }
}

pub(super) fn fork_vm_errno(err: VmError) -> errno::Errno {
    match err {
        VmError::OutOfFrames => errno::ENOMEM,
        _ => errno::EINVAL,
    }
}

pub(super) fn spawn_errno(err: sched::SpawnError) -> errno::Errno {
    match err {
        sched::SpawnError::NoThreadSlots | sched::SpawnError::NoStackSlots => errno::EAGAIN,
        sched::SpawnError::SchedulerNotInitialized
        | sched::SpawnError::UserThreadMustBeJoinable => errno::EINVAL,
    }
}

pub(super) fn exec_vm_errno(err: VmError) -> errno::Errno {
    match err {
        VmError::OutOfFrames => errno::ENOMEM,
        _ => errno::EINVAL,
    }
}

pub(super) fn exec_elf_errno(err: ElfLoadError) -> errno::Errno {
    match err {
        ElfLoadError::FrameAllocationFailed => errno::ENOMEM,
        _ => errno::ENOEXEC,
    }
}

pub(super) fn exec_user_copy_errno(err: UserCopyError) -> errno::Errno {
    match err {
        UserCopyError::NameTooLong => errno::ENAMETOOLONG,
        UserCopyError::TooLarge => errno::E2BIG,
        UserCopyError::OutOfMemory => errno::ENOMEM,
        _ => errno::EFAULT,
    }
}

pub(super) fn exec_arg_copy_errno(err: UserCopyError) -> errno::Errno {
    match err {
        UserCopyError::NameTooLong | UserCopyError::TooLarge => errno::E2BIG,
        UserCopyError::OutOfMemory => errno::ENOMEM,
        _ => errno::EFAULT,
    }
}

pub(super) fn wait_user_copy_errno(_err: UserCopyError) -> errno::Errno {
    errno::EFAULT
}
