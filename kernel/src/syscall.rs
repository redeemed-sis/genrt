use crate::{
    arch::{ActiveContext, SyscallRequest},
    console, errno,
    fs::{
        fd::{self, FdError},
        path::{self, PathError},
        ramfs,
    },
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
const _: () = assert!(USER_CPU_SET_BITS % 8 == 0);
const _: () = assert!(USER_CPU_SET_BYTES <= user::MAX_USER_COPY);

const O_RDONLY: usize = 0;
const O_WRONLY: usize = 1;
const O_RDWR: usize = 2;
const O_ACCMODE: usize = 0o3;
const O_CREAT: usize = 0o100;
const O_TRUNC: usize = 0o1000;
const O_APPEND: usize = 0o2000;
const O_DIRECTORY: usize = 0o200000;
const SUPPORTED_OPEN_FLAGS: usize = O_ACCMODE | O_CREAT | O_TRUNC | O_APPEND | O_DIRECTORY;

const DIRENT64_ALIGN: usize = 8;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;

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
/// IRQ entry never calls this API.
pub fn dispatch(
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
        SYS_CLOCK_NANOSLEEP => {
            sys_clock_nanosleep(context, request);
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

    let result = sys_read_file(fd, ptr, count);
    context.set_syscall_result(errno::syscall_ret(result));
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

fn sys_read_file(fd: usize, ptr: usize, count: usize) -> Result<usize, errno::Errno> {
    if count == 0 {
        return Ok(0);
    }
    if fd == fd::STDOUT || fd == fd::STDERR {
        return Err(errno::EBADF);
    }

    let max_len = count.min(user::MAX_USER_COPY);
    let chunk = process::current_fd_read_slice(fd, max_len).map_err(fd_errno)?;
    if chunk.is_empty() {
        return Ok(0);
    }

    user::copy_to_user(ptr, chunk).map_err(user_copy_errno)?;
    process::advance_current_fd_read(fd, chunk.len()).map_err(fd_errno)?;
    Ok(chunk.len())
}

fn sys_write(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let fd = request.arg(0);
    let ptr = request.arg(1);
    let len = request.arg(2);

    let result = (|| {
        if fd != fd::STDOUT && fd != fd::STDERR {
            if process::current_fd_is_open(fd).map_err(fd_errno)? {
                return Err(errno::ENOTSUP);
            }
            return Err(errno::EBADF);
        }
        if len == 0 {
            return Ok(0);
        }

        let len = len.min(user::MAX_USER_COPY);
        let mut buffer = [0u8; user::MAX_USER_COPY];
        user::copy_from_user(&mut buffer[..len], ptr).map_err(user_copy_errno)?;

        console::write_bytes(&buffer[..len]);

        Ok(len)
    })();

    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_open(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let path_ptr = request.arg(0);
    let flags = request.arg(1);
    let _mode = request.arg(2);

    let result = (|| {
        validate_open_flags(flags)?;
        let input = user::copy_path_cstr_from_user(path_ptr).map_err(path_copy_errno)?;
        let path = resolve_current_path(&input)?;
        match path.node {
            path::ResolvedNode::Directory { dir_index } => {
                process::open_current_ram_dir(dir_index).map_err(fd_errno)
            }
            path::ResolvedNode::File { file_index } => {
                if flags & O_DIRECTORY != 0 {
                    return Err(errno::ENOTDIR);
                }
                process::open_current_ram_file(file_index).map_err(fd_errno)
            }
        }
    })();

    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_close(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let fd = request.arg(0);
    let result = process::close_current_fd(fd).map(|()| 0).map_err(fd_errno);
    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_exit(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let code = request.arg(0);
    crate::debug!("syscall: exit code={code}");
    process::process_exit_current(context, code);
}

fn sys_fork(context: &mut ActiveContext<'_>) {
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
            if let Some(owner) = sched::current_thread_id() {
                crate::profile::begin(owner, deadline, early);
            }
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
    let path_ptr = request.arg(0);
    let argv_ptr = request.arg(1);
    let envp_ptr = request.arg(2);

    let result = (|| {
        let input = user::copy_path_cstr_from_user(path_ptr).map_err(path_copy_errno)?;
        let path = resolve_current_path(&input)?;
        let args = process::copy_exec_args_from_user(&path.absolute, argv_ptr, envp_ptr)?;
        process::execve_current(context, path, args)
    })();

    if let Err(errno) = result {
        context.set_syscall_result(-errno);
    }
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
    let fd = request.arg(0);
    let ptr = request.arg(1);
    let count = request.arg(2);
    let result = sys_getdents64_impl(fd, ptr, count);
    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_getdents64_impl(fd: usize, ptr: usize, count: usize) -> Result<usize, errno::Errno> {
    if count == 0 {
        return Err(errno::EINVAL);
    }

    let first_entry = process::current_fd_dir_entry_at(fd, 0).map_err(fd_errno)?;
    let max_len = count.min(user::MAX_USER_COPY);
    user::validate_user_write_range(ptr, max_len).map_err(user_copy_errno)?;

    let mut buffer = [0u8; user::MAX_USER_COPY];
    let mut written = 0usize;
    let mut emitted = 0usize;

    loop {
        let dir_entry = if emitted == 0 {
            first_entry
        } else {
            process::current_fd_dir_entry_at(fd, emitted).map_err(fd_errno)?
        };
        let Some(dir_entry) = dir_entry else {
            break;
        };
        let d_type = match dir_entry.entry.kind {
            ramfs::DirEntryKind::Directory => DT_DIR,
            ramfs::DirEntryKind::File => DT_REG,
        };
        let next_offset = dir_entry.offset.saturating_add(1) as i64;

        let Some(record_len) = encode_dirent64(
            &mut buffer[written..max_len],
            dir_entry.entry.ino,
            next_offset,
            d_type,
            dir_entry.entry.name,
        ) else {
            if written == 0 {
                return Err(errno::EINVAL);
            }
            break;
        };

        written += record_len;
        emitted += 1;
    }

    if written == 0 {
        return Ok(0);
    }

    user::copy_to_user(ptr, &buffer[..written]).map_err(user_copy_errno)?;
    process::advance_current_fd_dir_read(fd, emitted).map_err(fd_errno)?;
    Ok(written)
}

fn sys_chdir(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let path_ptr = request.arg(0);
    let result = (|| {
        let input = user::copy_path_cstr_from_user(path_ptr).map_err(path_copy_errno)?;
        let path = resolve_current_path(&input)?;
        match path.node {
            path::ResolvedNode::Directory { dir_index } => {
                process::set_current_cwd(dir_index).map_err(process_path_errno)?;
                Ok(0)
            }
            path::ResolvedNode::File { .. } => Err(errno::ENOTDIR),
        }
    })();
    context.set_syscall_result(errno::syscall_ret(result));
}

fn sys_getcwd(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let ptr = request.arg(0);
    let size = request.arg(1);
    let result = (|| {
        let cwd = process::current_cwd_path().map_err(process_path_errno)?;
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

fn validate_open_flags(flags: usize) -> Result<(), errno::Errno> {
    if flags & !SUPPORTED_OPEN_FLAGS != 0 {
        return Err(errno::EINVAL);
    }
    if flags & (O_CREAT | O_TRUNC) != 0 {
        return Err(errno::EROFS);
    }
    if flags & O_APPEND != 0 {
        return Err(errno::ENOTSUP);
    }

    match flags & O_ACCMODE {
        O_RDONLY => Ok(()),
        O_WRONLY | O_RDWR => Err(errno::ENOTSUP),
        _ => Err(errno::EINVAL),
    }
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

fn path_copy_errno(err: UserCopyError) -> errno::Errno {
    match err {
        UserCopyError::NameTooLong => errno::ENAMETOOLONG,
        UserCopyError::Empty => errno::ENOENT,
        UserCopyError::OutOfMemory => errno::ENOMEM,
        _ => errno::EFAULT,
    }
}

fn path_errno(err: PathError) -> errno::Errno {
    match err {
        PathError::Empty => errno::ENOENT,
        PathError::NotFound => errno::ENOENT,
        PathError::NotDirectory => errno::ENOTDIR,
        PathError::NameTooLong => errno::ENAMETOOLONG,
        PathError::InvalidBase => errno::EINVAL,
        PathError::NoMemory => errno::ENOMEM,
    }
}

fn process_path_errno(err: process::ProcessPathError) -> errno::Errno {
    match err {
        process::ProcessPathError::NoCurrentProcess
        | process::ProcessPathError::InvalidDirectory => errno::EINVAL,
    }
}

fn resolve_current_path(input: &[u8]) -> Result<path::ResolvedPath, errno::Errno> {
    let cwd_dir = process::current_cwd_dir().map_err(process_path_errno)?;
    path::resolve_existing_path(cwd_dir, input).map_err(path_errno)
}

fn fd_errno(err: FdError) -> errno::Errno {
    match err {
        FdError::BadFd => errno::EBADF,
        FdError::IsDirectory => errno::EISDIR,
        FdError::NotDirectory => errno::ENOTDIR,
        FdError::TooManyOpenFiles => errno::EMFILE,
        FdError::Unsupported => errno::ENOTSUP,
    }
}

fn encode_dirent64(dst: &mut [u8], ino: u64, off: i64, d_type: u8, name: &[u8]) -> Option<usize> {
    let raw_len = DIRENT64_HEADER_SIZE
        .checked_add(name.len())?
        .checked_add(1)?;
    let record_len = memory::align_up(raw_len, DIRENT64_ALIGN)?;
    if record_len > dst.len() || record_len > u16::MAX as usize || name.contains(&b'/') {
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
