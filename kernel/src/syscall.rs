use crate::{console, process};

pub const SYS_WRITE: usize = 1;
pub const SYS_EXIT: usize = 2;

const X0: usize = 0;
const X1: usize = 1;
const X2: usize = 2;
const X8: usize = 8;
const STDOUT: usize = 1;

pub fn dispatch(frame_words: *mut u64) {
    if frame_words.is_null() {
        panic!("syscall: null trap frame");
    }

    let nr = frame_word(frame_words, X8) as usize;
    match nr {
        SYS_WRITE => sys_write(frame_words),
        SYS_EXIT => sys_exit(frame_words),
        _ => {
            let elr = frame_word(frame_words, 32);
            crate::error!("syscall: unknown nr={nr} elr=0x{elr:x}; terminating current user task");
            crate::sched::on_thread_exit_sync(frame_words, usize::MAX);
        }
    }
}

fn sys_write(frame_words: *mut u64) {
    let fd = frame_word(frame_words, X0) as usize;
    let ptr = frame_word(frame_words, X1) as usize;
    let len = frame_word(frame_words, X2) as usize;

    if fd != STDOUT || !process::validate_user_buffer(ptr, len) {
        set_return(frame_words, -1);
        return;
    }

    let mut written = 0usize;
    while written < len {
        // SAFETY: `validate_user_buffer()` verified the range belongs to the
        // current TTBR0 user address space. This bring-up copy intentionally
        // relies on the active user mapping; a later milestone should replace
        // it with fault-aware `copy_from_user`.
        let byte = unsafe { (ptr as *const u8).add(written).read_volatile() };
        console::putc(byte);
        written += 1;
    }

    set_return(frame_words, written as isize);
}

fn sys_exit(frame_words: *mut u64) {
    let code = frame_word(frame_words, X0) as usize;
    crate::debug!("syscall: exit code={code}");
    crate::sched::on_thread_exit_sync(frame_words, code);
}

fn frame_word(frame_words: *mut u64, index: usize) -> u64 {
    // SAFETY: exception assembly passed a live TrapFrame storage pointer.
    unsafe { frame_words.add(index).read_volatile() }
}

fn set_return(frame_words: *mut u64, value: isize) {
    // SAFETY: x0 is the syscall return register in the saved TrapFrame.
    unsafe { frame_words.add(X0).write_volatile(value as u64) };
}
