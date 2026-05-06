const TASK_CALL_SLEEP_UNTIL: u64 = 1;
const TASK_CALL_MAILBOX_WAIT: u64 = 2;
const TASK_CALL_THREAD_EXIT: u64 = 3;
const TASK_CALL_THREAD_JOIN: u64 = 4;

use crate::task::ThreadId;

unsafe extern "C" {
    fn arch_task_call(request: *const core::ffi::c_void);
}

#[repr(C)]
struct TaskCallRequest {
    op: u64,
    args: TaskCallArgs,
}

#[repr(C)]
union TaskCallArgs {
    sleep_until: TaskCallSleepUntil,
    mailbox_wait: TaskCallMailboxWait,
    thread_exit: TaskCallThreadExit,
    thread_join: TaskCallThreadJoin,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct TaskCallSleepUntil {
    deadline: u64,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct TaskCallMailboxWait {
    mailbox: *const core::ffi::c_void,
    wait_kind: u64,
    timeout_enabled: u64,
    deadline: u64,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct TaskCallThreadExit {
    code: usize,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct TaskCallThreadJoin {
    index: usize,
    generation: u32,
}

impl TaskCallRequest {
    fn sleep_until(deadline: u64) -> Self {
        Self {
            op: TASK_CALL_SLEEP_UNTIL,
            args: TaskCallArgs {
                sleep_until: TaskCallSleepUntil { deadline },
            },
        }
    }

    fn mailbox_wait(
        mailbox: *const core::ffi::c_void,
        wait_kind: u64,
        timeout_deadline: Option<u64>,
    ) -> Self {
        let (timeout_enabled, deadline) = match timeout_deadline {
            Some(deadline) => (1, deadline),
            None => (0, 0),
        };

        Self {
            op: TASK_CALL_MAILBOX_WAIT,
            args: TaskCallArgs {
                mailbox_wait: TaskCallMailboxWait {
                    mailbox,
                    wait_kind,
                    timeout_enabled,
                    deadline,
                },
            },
        }
    }

    fn thread_exit(code: usize) -> Self {
        Self {
            op: TASK_CALL_THREAD_EXIT,
            args: TaskCallArgs {
                thread_exit: TaskCallThreadExit { code },
            },
        }
    }

    fn thread_join(id: ThreadId) -> Self {
        Self {
            op: TASK_CALL_THREAD_JOIN,
            args: TaskCallArgs {
                thread_join: TaskCallThreadJoin {
                    index: id.index(),
                    generation: id.generation(),
                },
            },
        }
    }
}

pub(crate) fn sleep_until_counter(deadline: u64) {
    let request = TaskCallRequest::sleep_until(deadline);
    // SAFETY: `arch_task_call()` enters a controlled synchronous exception path.
    // The architecture saves the current task frame and routes the typed request
    // back into `on_arch_task_call()`. The request lives on the current task's
    // stack and is consumed synchronously before this function can return.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
}

pub(crate) fn mailbox_wait(mailbox: *const core::ffi::c_void, wait_kind: u64) {
    let request = TaskCallRequest::mailbox_wait(mailbox, wait_kind, None);
    // SAFETY: same controlled synchronous exception path as sleep. The caller
    // passes an opaque pointer whose lifetime is validated by the mailbox owner.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
}

pub(crate) fn mailbox_wait_until_counter(
    mailbox: *const core::ffi::c_void,
    wait_kind: u64,
    deadline: u64,
) {
    let request = TaskCallRequest::mailbox_wait(mailbox, wait_kind, Some(deadline));
    // SAFETY: same controlled synchronous exception path as sleep. The timeout
    // deadline is an absolute architecture counter value consumed synchronously.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
}

pub(crate) fn thread_exit(code: usize) -> ! {
    let request = TaskCallRequest::thread_exit(code);
    // SAFETY: thread exit uses the same controlled synchronous exception path as
    // other scheduler task calls. The scheduler replaces the active frame with a
    // different runnable thread, so this call must not return to the exiting one.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
    panic!("task-call: thread_exit returned to exiting thread");
}

pub(crate) fn thread_join(id: ThreadId) {
    let request = TaskCallRequest::thread_join(id);
    // SAFETY: the join request is consumed synchronously by the controlled SVC
    // path before this stack frame can go away or the caller blocks.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
}

pub fn on_arch_task_call(active_frame_words: *mut u64, request: *const core::ffi::c_void) {
    if request.is_null() {
        panic!("task-call: null request");
    }

    // SAFETY: `arch_task_call()` passes a pointer to a live request on the
    // current task stack. The exception dispatcher consumes it synchronously
    // before returning or switching away from the task.
    let request = unsafe { &*request.cast::<TaskCallRequest>() };

    match request.op {
        TASK_CALL_SLEEP_UNTIL => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.sleep_until };
            crate::sched::on_sleep_sync(active_frame_words, args.deadline);
        }
        TASK_CALL_MAILBOX_WAIT => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.mailbox_wait };
            crate::ipc::on_mailbox_wait_sync(
                active_frame_words,
                args.mailbox.cast_mut(),
                args.wait_kind,
                if args.timeout_enabled != 0 {
                    Some(args.deadline)
                } else {
                    None
                },
            );
        }
        TASK_CALL_THREAD_EXIT => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.thread_exit };
            crate::sched::on_thread_exit_sync(active_frame_words, args.code);
        }
        TASK_CALL_THREAD_JOIN => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.thread_join };
            crate::sched::on_thread_join_sync(
                active_frame_words,
                ThreadId::new(args.index, args.generation),
            );
        }
        op => panic!("task-call: unknown operation {op}"),
    }
}
