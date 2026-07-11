pub type Errno = isize;

pub const EPERM: Errno = 1;
pub const ENOENT: Errno = 2;
pub const E2BIG: Errno = 7;
pub const ENOEXEC: Errno = 8;
pub const EBADF: Errno = 9;
pub const ECHILD: Errno = 10;
pub const EAGAIN: Errno = 11;
pub const ENOMEM: Errno = 12;
pub const EACCES: Errno = 13;
pub const EFAULT: Errno = 14;
pub const ENOTDIR: Errno = 20;
pub const EISDIR: Errno = 21;
pub const EINVAL: Errno = 22;
pub const EMFILE: Errno = 24;
pub const EROFS: Errno = 30;
pub const ENAMETOOLONG: Errno = 36;
pub const ENOSYS: Errno = 38;
pub const ENOTSUP: Errno = 95;

pub fn syscall_ret(result: Result<usize, Errno>) -> isize {
    match result {
        Ok(value) => value as isize,
        Err(errno) => -errno,
    }
}
