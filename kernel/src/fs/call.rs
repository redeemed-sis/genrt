//! Restartable EL0 VFS calls; no borrowed syscall frame survives a wait.

use super::{
    backend::{Kind, NAME_MAX, OpenFlags, Operation, Reply},
    fd::{Open, Transaction},
    vfs::{Resolver, namespace},
};
use crate::{
    arch::{ActiveContext, SyscallRequest},
    errno::{self, Errno},
    memory::user,
    process,
    sched::{self, CommitResult, WaitToken},
    sync::task::TaskWindow,
    syscall::*,
};
use alloc::boxed::Box;
use core::task::Poll;

enum Work {
    Open(Box<Open>),
    Io(Transaction),
    Resolve(Box<Resolver>),
}

/// Process-owned syscall restart state. Buffers, stages and OFD lease are owned.
pub(crate) struct PendingFileCall {
    request: SyscallRequest,
    work: Work,
    /// Exact blocked token; early commit clears it because scheduler consumed it.
    pub(crate) waited: Option<WaitToken>,
}

fn flags(raw: usize) -> Result<OpenFlags, Errno> {
    const ALLOWED: usize = 3 | 0o100 | 0o1000 | 0o2000 | 0o200000;
    if raw & !ALLOWED != 0 || raw & 3 == 3 {
        return Err(errno::EINVAL);
    }
    Ok(OpenFlags {
        read: raw & 3 != 1,
        write: raw & 3 != 0,
        create: raw & 0o100 != 0,
        truncate: raw & 0o1000 != 0,
        append: raw & 0o2000 != 0,
        directory: raw & 0o200000 != 0,
    })
}

fn initialize(request: SyscallRequest) -> Result<PendingFileCall, Errno> {
    let work = match request.number() {
        SYS_OPEN | SYS_CHDIR | SYS_EXECVE => {
            let open_flags = if request.number() == SYS_OPEN {
                let flags = flags(request.arg(1))?;
                // One user thread owns this table, so this slot stays free over
                // waits. No backend side effect may precede this capacity check.
                process::current_next_fd()?;
                Some(flags)
            } else {
                None
            };
            let path = user::copy_path_cstr_from_user(request.arg(0)).map_err(path_copy_errno)?;
            let cwd = process::current_cwd_dir()?;
            if let Some(flags) = open_flags {
                Work::Open(Box::new(Open::new(cwd, &path, flags)?))
            } else {
                Work::Resolve(Box::new(Resolver::new(cwd, &path, false)?))
            }
        }
        SYS_READ | SYS_WRITE | SYS_GETDENTS64 => {
            let count = request.arg(2).min(user::MAX_USER_COPY);
            if request.number() == SYS_GETDENTS64 && count == 0 {
                return Err(errno::EINVAL);
            }
            let description = process::current_fd(request.arg(0))?;
            let operation = match request.number() {
                SYS_WRITE => Operation::Write {
                    offset: 0,
                    append: false,
                },
                SYS_READ => Operation::Read {
                    offset: 0,
                    limit: count,
                },
                _ => Operation::Readdir { offset: 0 },
            };
            let transaction = Transaction::new(description, operation, count)?;
            // Output validation uses the actual completed byte/record length,
            // and happens before offset commit, not before submitting a read.
            Work::Io(transaction)
        }
        _ => return Err(errno::ENOSYS),
    };
    Ok(PendingFileCall {
        request,
        work,
        waited: None,
    })
}

fn poll(
    call: &mut PendingFileCall,
    context: &mut ActiveContext<'_>,
    token: WaitToken,
) -> Poll<Result<usize, Errno>> {
    let request = call.request;
    match &mut call.work {
        Work::Open(open) => {
            let description = match open.poll(namespace()?, Some(token)) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result?,
            };
            let mut description = Some(description);
            Poll::Ready(process::install_current_fd(&mut description))
        }
        Work::Resolve(resolver) => {
            let location = match resolver.poll(namespace()?, Some(token)) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result?,
            };
            let result = if request.number() == SYS_CHDIR {
                process::set_current_cwd(location).map(|()| 0)
            } else {
                let args = process::copy_exec_args_from_user(
                    location.path(),
                    request.arg(1),
                    request.arg(2),
                )?;
                process::execve_current(context, location, args).map(|()| 0)
            };
            Poll::Ready(result)
        }
        Work::Io(transaction) => {
            if request.arg(2) == 0 && matches!(request.number(), SYS_READ | SYS_WRITE) {
                return Poll::Ready(Ok(0));
            }
            match transaction.prepare(Some(token), |buffer| {
                user::copy_from_user(buffer, request.arg(1)).map_err(copy_errno)
            }) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result?,
            }
            let reply = match transaction.poll(Some(token)) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result?,
            };
            let count = match reply {
                Reply::Io { count, .. } => {
                    if request.number() == SYS_READ {
                        user::copy_to_user(request.arg(1), &transaction.buffer()[..count])
                            .map_err(copy_errno)?;
                    }
                    count
                }
                Reply::Entry(None) => return Poll::Ready(Ok(0)),
                Reply::Entry(Some(entry)) => {
                    if entry.name.is_empty()
                        || entry.name.len() > NAME_MAX
                        || entry.name.contains(&0)
                        || entry.name.contains(&b'/')
                    {
                        return Poll::Ready(Err(errno::EIO));
                    }
                    // A single record is a valid bounded partial directory read.
                    // It also keeps an unencodable record out of committed offsets.
                    let kind = if entry.node.kind == Kind::Directory {
                        4
                    } else {
                        8
                    };
                    let offset = i64::try_from(transaction.next_offset().ok_or(errno::EOVERFLOW)?)
                        .map_err(|_| errno::EOVERFLOW)?;
                    let length = encode_dirent64(
                        transaction.output_buffer(),
                        entry.node.id,
                        offset,
                        kind,
                        &entry.name,
                    )
                    .ok_or(errno::EINVAL)?;
                    user::copy_to_user(request.arg(1), &transaction.buffer()[..length])
                        .map_err(copy_errno)?;
                    length
                }
                _ => return Poll::Ready(Err(errno::EIO)),
            };
            transaction.commit();
            Poll::Ready(Ok(count))
        }
    }
}

/// Run/resume a VFS syscall, publishing retained ownership before scheduler commit.
/// # Arguments
/// * `context` - Exclusive live frame; never retained in a continuation.
/// * `request` - Decoded syscall, verified unchanged on restart.
/// # Returns
/// Nothing; result stored, new exec frame installed, or exact wait committed.
/// Finite callbacks/allocation/usercopy run in TaskWindow; commit runs outside it.
pub(crate) fn dispatch(context: &mut ActiveContext<'_>, request: SyscallRequest) {
    let pending = process::take_file_call();
    let mut call = if let Some(mut call) = pending {
        assert_eq!(call.request, request, "vfs: restarted request changed");
        if let Some(token) = call.waited.take() {
            sched::finish_wait(token).expect("vfs: resumed without exact completion");
        }
        call
    } else {
        let _window = TaskWindow::enter();
        match initialize(request) {
            Ok(call) => call,
            Err(err) => {
                context.set_syscall_result(-err);
                return;
            }
        }
    };
    let prepared = sched::prepare_wait();
    let (result, pending) = {
        let _window = TaskWindow::enter();
        let result = poll(&mut call, context, prepared.token());
        if result.is_ready() {
            drop(call);
            (result, None)
        } else {
            (result, Some(call))
        }
    };
    match result {
        Poll::Ready(result) => {
            let _ = sched::cancel_wait(prepared);
            if request.number() != SYS_EXECVE || result.is_err() {
                context.set_syscall_result(errno::syscall_ret(result));
            }
        }
        Poll::Pending => {
            let mut call = pending.unwrap();
            call.waited = Some(prepared.token());
            process::store_file_call(call);
            context.restart_current_syscall();
            match sched::commit_wait(context, prepared) {
                CommitResult::Blocked(_) => {}
                CommitResult::Early(_) => process::early_file_call(),
                CommitResult::Stale => panic!("vfs: pending wait became stale"),
            }
        }
    }
}

fn copy_errno(error: user::UserCopyError) -> Errno {
    match error {
        user::UserCopyError::OutOfMemory => errno::ENOMEM,
        _ => errno::EFAULT,
    }
}

fn path_copy_errno(error: user::UserCopyError) -> Errno {
    match error {
        user::UserCopyError::Empty => errno::ENOENT,
        user::UserCopyError::NameTooLong => errno::ENAMETOOLONG,
        _ => copy_errno(error),
    }
}
