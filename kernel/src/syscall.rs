use crate::{
    console, errno,
    fs::{
        fd::{self, FdError},
        path::{self, PathError},
        ramfs,
    },
    memory::user::{self, UserCopyError},
    process,
};

pub const SYS_READ: usize = 0;
pub const SYS_WRITE: usize = 1;
pub const SYS_EXIT: usize = 2;
pub const SYS_OPEN: usize = 3;
pub const SYS_CLOSE: usize = 4;

const X0: usize = 0;
const X1: usize = 1;
const X2: usize = 2;
const X8: usize = 8;

const O_RDONLY: usize = 0;
const O_WRONLY: usize = 1;
const O_RDWR: usize = 2;
const O_ACCMODE: usize = 0o3;
const O_CREAT: usize = 0o100;
const O_TRUNC: usize = 0o1000;
const O_APPEND: usize = 0o2000;
const SUPPORTED_OPEN_FLAGS: usize = O_ACCMODE | O_CREAT | O_TRUNC | O_APPEND;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DispatchError {
    UnknownSyscall(usize),
}

pub fn dispatch(frame_words: *mut u64) -> Result<(), DispatchError> {
    if frame_words.is_null() {
        panic!("syscall: null trap frame");
    }

    let nr = frame_word(frame_words, X8) as usize;
    match nr {
        SYS_READ => {
            sys_read(frame_words);
            Ok(())
        }
        SYS_WRITE => {
            sys_write(frame_words);
            Ok(())
        }
        SYS_EXIT => {
            sys_exit(frame_words);
            Ok(())
        }
        SYS_OPEN => {
            sys_open(frame_words);
            Ok(())
        }
        SYS_CLOSE => {
            sys_close(frame_words);
            Ok(())
        }
        _ => Err(DispatchError::UnknownSyscall(nr)),
    }
}

fn sys_read(frame_words: *mut u64) {
    let fd = frame_word(frame_words, X0) as usize;
    let ptr = frame_word(frame_words, X1) as usize;
    let count = frame_word(frame_words, X2) as usize;

    let result = (|| {
        if count == 0 {
            return Ok(0);
        }

        let max_len = count.min(user::MAX_USER_COPY);
        let chunk = process::current_fd_read_slice(fd, max_len).map_err(fd_errno)?;
        if chunk.is_empty() {
            return Ok(0);
        }

        user::copy_to_user(ptr, chunk).map_err(user_copy_errno)?;
        process::advance_current_fd_read(fd, chunk.len()).map_err(fd_errno)?;
        Ok(chunk.len())
    })();

    set_return(frame_words, errno::syscall_ret(result));
}

fn sys_write(frame_words: *mut u64) {
    let fd = frame_word(frame_words, X0) as usize;
    let ptr = frame_word(frame_words, X1) as usize;
    let len = frame_word(frame_words, X2) as usize;

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

        for byte in &buffer[..len] {
            console::putc(*byte);
        }

        Ok(len)
    })();

    set_return(frame_words, errno::syscall_ret(result));
}

fn sys_open(frame_words: *mut u64) {
    let path_ptr = frame_word(frame_words, X0) as usize;
    let flags = frame_word(frame_words, X1) as usize;
    let _mode = frame_word(frame_words, X2) as usize;

    let result = (|| {
        validate_open_flags(flags)?;
        let path = user::copy_path_cstr_from_user(path_ptr).map_err(path_copy_errno)?;
        let path = path::root_relative(&path).map_err(path_errno)?;
        let file_index = ramfs::lookup(&path).ok_or(errno::ENOENT)?;
        process::open_current_ram_file(file_index).map_err(fd_errno)
    })();

    set_return(frame_words, errno::syscall_ret(result));
}

fn sys_close(frame_words: *mut u64) {
    let fd = frame_word(frame_words, X0) as usize;
    let result = process::close_current_fd(fd).map(|()| 0).map_err(fd_errno);
    set_return(frame_words, errno::syscall_ret(result));
}

fn sys_exit(frame_words: *mut u64) {
    let code = frame_word(frame_words, X0) as usize;
    crate::debug!("syscall: exit code={code}");
    process::process_exit_current(frame_words, code);
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
        UserCopyError::Empty => errno::EINVAL,
        UserCopyError::OutOfMemory => errno::ENOMEM,
        _ => errno::EFAULT,
    }
}

fn path_errno(err: PathError) -> errno::Errno {
    match err {
        PathError::Empty => errno::EINVAL,
        PathError::NoMemory => errno::ENOMEM,
    }
}

fn fd_errno(err: FdError) -> errno::Errno {
    match err {
        FdError::BadFd => errno::EBADF,
        FdError::TooManyOpenFiles => errno::EMFILE,
        FdError::Unsupported => errno::ENOTSUP,
    }
}

fn frame_word(frame_words: *mut u64, index: usize) -> u64 {
    // SAFETY: exception assembly passed a live TrapFrame storage pointer.
    unsafe { frame_words.add(index).read_volatile() }
}

fn set_return(frame_words: *mut u64, value: isize) {
    // SAFETY: x0 is the syscall return register in the saved TrapFrame.
    unsafe { frame_words.add(X0).write_volatile(value as u64) };
}
