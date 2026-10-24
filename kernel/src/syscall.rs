use crate::{
    arch::{ActiveContext, SyscallRequest},
    console, errno,
    fs::{self, fd},
    memory::{
        self,
        user::{self, UserCopyError},
    },
    power, process, sched, time,
};

pub const SYS_READ: usize = 0;
pub const SYS_WRITE: usize = 1;
pub const SYS_EXIT: usize = 2;
pub const SYS_OPEN: usize = 3;
pub const SYS_CLOSE: usize = 4;
pub const SYS_FORK: usize = 5;
pub const SYS_EXECVE: usize = 6;
pub const SYS_WAITPID: usize = 7;
/// Read Linux-like `dirent64` records from a directory file descriptor.
pub const SYS_GETDENTS64: usize = 8;
/// Change the current user process working directory.
pub const SYS_CHDIR: usize = 9;
/// Copy the current user process canonical working directory to userspace.
pub const SYS_GETCWD: usize = 10;
/// Set the current process's one-shot child CPU affinity for `fork`.
pub const SYS_SCHED_SETFORKAFFINITY: usize = 11;
/// Return the immutable CPU affinity mask of one process.
pub const SYS_SCHED_GETAFFINITY: usize = 12;
/// Perform a Linux-shaped terminal system restart or power-off request.
pub const SYS_REBOOT: usize = 13;
/// Read the current value of a supported POSIX-like clock.
pub const SYS_CLOCK_GETTIME: usize = 14;
/// Read the effective resolution of a supported POSIX-like clock.
pub const SYS_CLOCK_GETRES: usize = 15;
/// Block until a relative or absolute POSIX-like clock deadline.
pub const SYS_CLOCK_NANOSLEEP: usize = 16;
/// Control an optional bounded wake profiling session.
pub const SYS_PROFILE_CONTROL: usize = 17;
/// Read a frozen optional wake profiling session.
pub const SYS_PROFILE_READ: usize = 18;

const CLOCK_MONOTONIC: isize = 1;
const TIMER_ABSTIME: usize = 1;
const NANOSECONDS_PER_SECOND: i64 = 1_000_000_000;
const USER_TIMESPEC_FIELD_SIZE: usize = core::mem::size_of::<i64>();
const USER_TIMESPEC_SIZE: usize = USER_TIMESPEC_FIELD_SIZE * 2;

// Stable userspace ABI capacity, independent of bounded kernel CPU storage.
const USER_CPU_SET_BITS: usize = 1024;
const USER_CPU_SET_BYTES: usize = USER_CPU_SET_BITS / 8;
const DIRENT64_ALIGN: usize = 8;
const _: () = assert!(USER_CPU_SET_BITS % 8 == 0);
const _: () = assert!(USER_CPU_SET_BYTES <= user::MAX_USER_COPY);

/// Fixed-width prefix of the userspace `genrt_dirent64` ABI record.
///
/// `packed` keeps the flexible `d_name[]` payload immediately after `d_type`
/// instead of including the trailing padding of a standalone C structure. The
/// integer fields use the target's native byte order, matching userspace built
/// for the same target architecture.
#[repr(C, packed)]
#[derive(Copy, Clone)]
struct Dirent64Header {
    ino: u64,
    off: i64,
    record_len: u16,
    entry_type: u8,
}

const DIRENT64_HEADER_SIZE: usize = core::mem::size_of::<Dirent64Header>();
const _: [(); 19] = [(); DIRENT64_HEADER_SIZE];

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DispatchError {
    UnknownSyscall(usize),
}

/// Dispatch one architecture-decoded userspace syscall.
///
/// # Arguments
///
/// * `context` - Exclusive live exception context used for results, blocking,
///   process transitions, and scheduler handoff.
/// * `request` - Architecture-neutral syscall number and six arguments.
///
/// # Returns
///
/// Returns `Ok(())` after a known syscall is handled. Blocking or terminating
/// syscalls may transfer control through the scheduler before this function
/// can return normally.
///
/// # Errors
///
/// Returns [`DispatchError::UnknownSyscall`] with the unrecognized syscall
/// number without modifying `context` or performing a syscall operation.
///
/// Individual handlers may allocate, copy userspace memory, or block only in
/// synchronous thread/syscall context. Dispatch itself adds no allocation, and
/// IRQ entry never calls this API. Clock sleep takes a separate path so other
/// handlers' temporary buffers do not enlarge its stack frame.
pub fn dispatch(
    context: &mut ActiveContext<'_>,
    request: SyscallRequest,
) -> Result<(), DispatchError> {
    if request.number() == SYS_CLOCK_NANOSLEEP {
        sys_clock_nanosleep(context, request);
        return Ok(());
    }
    dispatch_other(context, request)
}

// Keep filesystem and buffered-I/O stack storage out of the clock-sleep path.
#[inline(never)]
fn dispatch_other(
    context: &mut ActiveContext<'_>,
    request: SyscallRequest,
) -> Result<(), DispatchError> {
    let nr = request.number();
    match nr {
        SYS_READ => {
            sys_read(context, request);
            Ok(())
        }
        SYS_WRITE => {
            sys_write(context, request);
            Ok(())
        }
        SYS_EXIT => {
            sys_exit(context, request);
            Ok(())
        }
        SYS_OPEN => {
            sys_open(context, request);
            Ok(())
        }
        SYS_CLOSE => {
            sys_close(context, request);
            Ok(())
        }
        SYS_FORK => {
            sys_fork(context);
            Ok(())
        }
        SYS_EXECVE => {
            sys_execve(context, request);
            Ok(())
        }
        SYS_WAITPID => {
            sys_waitpid(context, request);
            Ok(())
        }
        SYS_GETDENTS64 => {
            sys_getdents64(context, request);
            Ok(())
        }
        SYS_CHDIR => {
            sys_chdir(context, request);
            Ok(())
        }
        SYS_GETCWD => {
            sys_getcwd(context, request);
            Ok(())
        }
        SYS_SCHED_SETFORKAFFINITY => {
            sys_sched_setforkaffinity(context, request);
            Ok(())
        }
        SYS_SCHED_GETAFFINITY => {
            sys_sched_getaffinity(context, request);
            Ok(())
        }
        SYS_REBOOT => {
            sys_reboot(context, request);
            Ok(())
        }
        SYS_CLOCK_GETTIME => {
            sys_clock_gettime(context, request);
            Ok(())
        }
        SYS_CLOCK_GETRES => {
            sys_clock_getres(context, request);
            Ok(())
        }
        SYS_PROFILE_CONTROL | SYS_PROFILE_READ => {
            #[cfg(feature = "wake-profile")]
            sys_profile(context, request);
            #[cfg(not(feature = "wake-profile"))]
            context.set_syscall_result(-errno::ENOSYS);
            Ok(())
        }
        _ => Err(DispatchError::UnknownSyscall(nr)),
    }
}

fn sys_read(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let fd = request.arg(0);
    let ptr = request.arg(1);
    let count = request.arg(2);

    if fd == fd::STDIN {
        match sys_read_stdin(context, ptr, count) {
            ReadAction::Return(result) => {
                context.set_syscall_result(errno::syscall_ret(result));
            }
            ReadAction::Blocked => {}
        }
        return;
    }

    fs::call::dispatch(context, request);
}

enum ReadAction {
    Return(Result<usize, errno::Errno>),
    Blocked,
}

fn sys_read_stdin(context: &mut ActiveContext<'_>, ptr: usize, count: usize) -> ReadAction {
    if count == 0 {
        return ReadAction::Return(Ok(0));
    }

    let max_len = count.min(user::MAX_USER_COPY);
    if let Err(err) = user::validate_user_write_range(ptr, max_len).map_err(user_copy_errno) {
        return ReadAction::Return(Err(err));
    }

    let mut buffer = [0u8; user::MAX_USER_COPY];
    let len = console::read_stdin(&mut buffer[..max_len]);
    if len != 0 {
        let result = user::copy_to_user(ptr, &buffer[..len])
            .map(|()| len)
            .map_err(user_copy_errno);
        return ReadAction::Return(result);
    }

    match console::block_current_stdin_read_if_empty(context) {
        Ok(true) => ReadAction::Blocked,
        Ok(false) => {
            let len = console::read_stdin(&mut buffer[..max_len]);
            if len != 0 {
                let result = user::copy_to_user(ptr, &buffer[..len])
                    .map(|()| len)
                    .map_err(user_copy_errno);
                ReadAction::Return(result)
            } else {
                ReadAction::Return(Err(errno::EFAULT))
            }
        }
        Err(()) => ReadAction::Return(Err(errno::ENOTSUP)),
    }
}

fn sys_write(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    if request.arg(0) != fd::STDOUT && request.arg(0) != fd::STDERR {
        fs::call::dispatch(context, request);
        return;
    }
    let result = (|| {
        let len = request.arg(2).min(user::MAX_USER_COPY);
        if len == 0 {
            return Ok(0);
        }
        let mut buffer = [0u8; user::MAX_USER_COPY];
        user::copy_from_user(&mut buffer[..len], request.arg(1)).map_err(user_copy_errno)?;
        console::write_bytes(&buffer[..len]);
        Ok(len)
    })();
    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_open(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    fs::call::dispatch(context, request);
}

fn sys_close(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let fd = request.arg(0);
    let _window = crate::sync::task::TaskWindow::enter();
    let result = process::close_current_fd(fd).map(|()| 0);
    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_exit(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let code = request.arg(0);
    crate::debug!("syscall: exit code={code}");
    process::process_exit_current(context, code);
}

fn sys_fork(context: &mut ActiveContext<'_>) {
    let _window = crate::sync::task::TaskWindow::enter();
    let result = process::fork_current(context);
    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_sched_setforkaffinity(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let result = process::set_fork_affinity_current(request.arg(0) as isize);
    context.set_syscall_result(errno::syscall_ret(result.map(|()| 0)));
}

fn sys_sched_getaffinity(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let raw_pid = request.arg(0) as isize;
    let cpusetsize = request.arg(1);
    let mask_ptr = request.arg(2);

    let result = (|| {
        if cpusetsize < USER_CPU_SET_BYTES {
            return Err(errno::EINVAL);
        }

        let owner = process::process_affinity(raw_pid)?;
        let cpu = owner.index();
        if cpu >= USER_CPU_SET_BITS {
            panic!("syscall: process owner exceeds userspace CPU set ABI");
        }

        let mut mask = [0u8; USER_CPU_SET_BYTES];
        mask[cpu / 8] = 1 << (cpu % 8);
        user::copy_to_user(mask_ptr, &mask).map_err(user_copy_errno)?;
        Ok(0)
    })();

    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_reboot(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    // The Linux-compatible fourth argument is reserved for command-specific
    // data. The supported restart and power-off commands do not consume it.
    let _arg = request.arg(3);
    let result = power::parse_linux_reboot(request.arg(0), request.arg(1), request.arg(2))
        .and_then(power::request);
    match result {
        Ok(never) => match never {},
        Err(err) => context.set_syscall_result(-err),
    }
}

fn sys_clock_gettime(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let result = (|| {
        validate_clock_id(request.arg(0) as isize)?;
        let now = time::counter_to_monotonic_time(time::now_counter()).map_err(time_errno)?;
        copy_timespec_to_user(request.arg(1), now)?;
        Ok(0)
    })();
    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_clock_getres(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let result = (|| {
        validate_clock_id(request.arg(0) as isize)?;
        let result_ptr = request.arg(1);
        if result_ptr != 0 {
            let resolution = time::monotonic_resolution().map_err(time_errno)?;
            copy_timespec_to_user(result_ptr, resolution)?;
        }
        Ok(0)
    })();
    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_clock_nanosleep(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    // `rem` stays in the ABI for future signal interruption. Without signals,
    // successful sleeps cannot report a remainder, so the pointer is neither
    // read nor validated yet.
    let _remaining_ptr = request.arg(3);
    let result = (|| {
        validate_clock_id(request.arg(0) as isize)?;
        let flags = request.arg(1);
        if flags & !TIMER_ABSTIME != 0 {
            return Err(errno::EINVAL);
        }

        let requested = copy_timespec_from_user(request.arg(2))?;
        let seconds = u64::try_from(requested.seconds).map_err(|_| errno::EINVAL)?;
        let nanoseconds = u32::try_from(requested.nanoseconds).map_err(|_| errno::EINVAL)?;
        if requested.nanoseconds >= NANOSECONDS_PER_SECOND {
            return Err(errno::EINVAL);
        }

        let deadline = if flags == TIMER_ABSTIME {
            time::monotonic_time_to_counter(seconds, nanoseconds).map_err(time_errno)?
        } else {
            let duration =
                time::monotonic_duration_to_counts(seconds, nanoseconds).map_err(time_errno)?;
            if duration == 0 {
                return Ok(None);
            }
            time::now_counter()
                .checked_add(duration)
                .ok_or(errno::EOVERFLOW)?
        };

        let early = deadline <= time::now_counter();
        #[cfg(feature = "wake-profile")]
        if flags == TIMER_ABSTIME {
            crate::profile::begin(deadline, early);
        }
        if early {
            return Ok(None);
        }
        Ok(Some(deadline))
    })();

    match result {
        Ok(Some(deadline)) => {
            // The blocked frame resumes directly in EL0 after this SVC. Store
            // success before scheduler handoff; automatic wait consumption
            // clears the exact token when the deadline completes.
            context.set_syscall_result(0);
            let _ = sched::sleep_user_until_counter(context, deadline);
        }
        Ok(None) => context.set_syscall_result(0),
        Err(err) => context.set_syscall_result(-err),
    }
}

#[cfg(feature = "wake-profile")]
fn sys_profile(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let result = (|| {
        // Process reverse lookup validates the current exact main thread.
        process::process_affinity(0)?;
        let owner = sched::current_thread_id().ok_or(errno::EPERM)?;
        if request.number() == SYS_PROFILE_CONTROL {
            let command = request.arg(0);
            let config = if command == 1 {
                let mut bytes = [0u8; 16];
                user::copy_from_user(&mut bytes, request.arg(1)).map_err(user_copy_errno)?;
                let fields: [u32; 4] = core::array::from_fn(|i| {
                    u32::from_le_bytes(core::array::from_fn(|j| bytes[i * 4 + j]))
                });
                Some(crate::profile::Config {
                    mode: fields[0],
                    max_records: fields[1],
                    start_phase: fields[2],
                    end_phase: fields[3],
                })
            } else {
                None
            };
            crate::profile::control(owner, command, config)
        } else {
            let size = request.arg(1).min(user::MAX_USER_COPY);
            let mut chunk = [0u8; user::MAX_USER_COPY];
            let len = crate::profile::read(owner, request.arg(2), &mut chunk[..size])?;
            // Frozen bytes staged under the leaf; all usercopy is outside it.
            if len != 0 {
                user::copy_to_user(request.arg(0), &chunk[..len]).map_err(user_copy_errno)?;
            }
            Ok(len)
        }
    })();
    context.set_syscall_result(errno::syscall_ret(result));
}

#[derive(Copy, Clone)]
struct UserTimespec {
    seconds: i64,
    nanoseconds: i64,
}

fn validate_clock_id(clock_id: isize) -> Result<(), errno::Errno> {
    if clock_id == CLOCK_MONOTONIC {
        Ok(())
    } else {
        Err(errno::EINVAL)
    }
}

fn copy_timespec_from_user(ptr: usize) -> Result<UserTimespec, errno::Errno> {
    let mut bytes = [0u8; USER_TIMESPEC_SIZE];
    user::copy_from_user(&mut bytes, ptr).map_err(user_copy_errno)?;
    let seconds = i64::from_ne_bytes(core::array::from_fn(|index| bytes[index]));
    let nanoseconds = i64::from_ne_bytes(core::array::from_fn(|index| {
        bytes[USER_TIMESPEC_FIELD_SIZE + index]
    }));
    Ok(UserTimespec {
        seconds,
        nanoseconds,
    })
}

fn copy_timespec_to_user(ptr: usize, value: time::MonotonicTime) -> Result<(), errno::Errno> {
    let seconds = i64::try_from(value.seconds).map_err(|_| errno::EOVERFLOW)?;
    let mut bytes = [0u8; USER_TIMESPEC_SIZE];
    bytes[..USER_TIMESPEC_FIELD_SIZE].copy_from_slice(&seconds.to_ne_bytes());
    bytes[USER_TIMESPEC_FIELD_SIZE..].copy_from_slice(&(value.nanoseconds as i64).to_ne_bytes());
    user::copy_to_user(ptr, &bytes).map_err(user_copy_errno)
}

fn time_errno(err: time::TimeConversionError) -> errno::Errno {
    match err {
        time::TimeConversionError::InvalidFrequency => errno::EINVAL,
        time::TimeConversionError::Overflow => errno::EOVERFLOW,
    }
}

fn sys_execve(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    fs::call::dispatch(context, request);
}

fn sys_waitpid(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let pid = request.arg(0) as isize;
    let status_ptr = request.arg(1);
    let options = request.arg(2);

    if options != 0 {
        context.set_syscall_result(-errno::ENOTSUP);
        return;
    }

    match process::waitpid_current(context, pid, status_ptr) {
        process::WaitPidAction::Return(result) => {
            context.set_syscall_result(errno::syscall_ret(result));
        }
        process::WaitPidAction::Blocked => {}
    }
}

fn sys_getdents64(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    fs::call::dispatch(context, request);
}

fn sys_chdir(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    fs::call::dispatch(context, request);
}

fn sys_getcwd(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let _window = crate::sync::task::TaskWindow::enter();
    let ptr = request.arg(0);
    let size = request.arg(1);
    let result = (|| {
        let location = process::current_cwd_dir()?;
        let cwd = location.path();
        let required = cwd.len().checked_add(1).ok_or(errno::ERANGE)?;
        if size < required {
            return Err(errno::ERANGE);
        }

        user::copy_cstr_to_user(ptr, cwd, crate::config::GENRT_PATH_MAX + 1)
            .map_err(user_copy_errno)?;
        Ok(required)
    })();
    context.set_syscall_result(errno::syscall_ret(result));
}

fn user_copy_errno(err: UserCopyError) -> errno::Errno {
    match err {
        UserCopyError::TooLarge => errno::EINVAL,
        UserCopyError::NameTooLong => errno::ENAMETOOLONG,
        UserCopyError::Empty => errno::EINVAL,
        UserCopyError::OutOfMemory => errno::ENOMEM,
        UserCopyError::AddressOverflow
        | UserCopyError::NotUserRange
        | UserCopyError::NotMapped
        | UserCopyError::NotReadable
        | UserCopyError::NotWritable
        | UserCopyError::NoCurrentAddressSpace => errno::EFAULT,
    }
}

/// Encode one bounded native dirent record without user access or allocation.
/// # Arguments
/// * `dst` - Owned kernel output staging.
/// * `ino` - Instance-local inode number.
/// * `off` - Next directory cursor.
/// * `d_type` - ABI directory entry type.
/// * `name` - Immediate child component.
/// # Returns
/// Record length, or none for invalid names or insufficient staging.
pub(crate) fn encode_dirent64(
    dst: &mut [u8],
    ino: u64,
    off: i64,
    d_type: u8,
    name: &[u8],
) -> Option<usize> {
    let raw_len = DIRENT64_HEADER_SIZE
        .checked_add(name.len())?
        .checked_add(1)?;
    let record_len = memory::align_up(raw_len, DIRENT64_ALIGN)?;
    if record_len > dst.len()
        || record_len > u16::MAX as usize
        || name.is_empty()
        || name.contains(&0)
        || name.contains(&b'/')
    {
        return None;
    }

    for byte in &mut dst[..record_len] {
        *byte = 0;
    }
    let header = Dirent64Header {
        ino,
        off,
        record_len: record_len as u16,
        entry_type: d_type,
    };
    // SAFETY: the size check above guarantees space for the complete header.
    // `write_unaligned` does not require the byte slice to satisfy the header's
    // alignment and writes native-endian fields for the current target ABI.
    unsafe {
        core::ptr::write_unaligned(dst.as_mut_ptr().cast::<Dirent64Header>(), header);
    }
    dst[DIRENT64_HEADER_SIZE..DIRENT64_HEADER_SIZE + name.len()].copy_from_slice(name);
    Some(record_len)
}
