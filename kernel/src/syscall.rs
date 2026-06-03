use crate::{console, memory::user, process};

pub const SYS_WRITE: usize = 1;
pub const SYS_EXIT: usize = 2;

const X0: usize = 0;
const X1: usize = 1;
const X2: usize = 2;
const X8: usize = 8;
const STDOUT: usize = 1;

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
        SYS_WRITE => {
            sys_write(frame_words);
            Ok(())
        }
        SYS_EXIT => {
            sys_exit(frame_words);
            Ok(())
        }
        _ => Err(DispatchError::UnknownSyscall(nr)),
    }
}

fn sys_write(frame_words: *mut u64) {
    let fd = frame_word(frame_words, X0) as usize;
    let ptr = frame_word(frame_words, X1) as usize;
    let len = frame_word(frame_words, X2) as usize;

    if fd != STDOUT || len > user::MAX_USER_COPY {
        set_return(frame_words, -1);
        return;
    }
    if len == 0 {
        set_return(frame_words, 0);
        return;
    }

    let mut buffer = [0u8; user::MAX_USER_COPY];
    if user::copy_from_user(&mut buffer[..len], ptr).is_err() {
        set_return(frame_words, -1);
        return;
    }

    for byte in &buffer[..len] {
        console::putc(*byte);
    }

    set_return(frame_words, len as isize);
}

fn sys_exit(frame_words: *mut u64) {
    let code = frame_word(frame_words, X0) as usize;
    crate::debug!("syscall: exit code={code}");
    process::process_exit_current(frame_words, code);
}

fn frame_word(frame_words: *mut u64, index: usize) -> u64 {
    // SAFETY: exception assembly passed a live TrapFrame storage pointer.
    unsafe { frame_words.add(index).read_volatile() }
}

fn set_return(frame_words: *mut u64, value: isize) {
    // SAFETY: x0 is the syscall return register in the saved TrapFrame.
    unsafe { frame_words.add(X0).write_volatile(value as u64) };
}
