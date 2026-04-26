const TASK_CALL_SLEEP_UNTIL: u64 = 1;
const TASK_CALL_MAILBOX_WAIT: u64 = 2;

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

    fn mailbox_wait(mailbox: *const core::ffi::c_void, wait_kind: u64) -> Self {
        Self {
            op: TASK_CALL_MAILBOX_WAIT,
            args: TaskCallArgs {
                mailbox_wait: TaskCallMailboxWait { mailbox, wait_kind },
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
    let request = TaskCallRequest::mailbox_wait(mailbox, wait_kind);
    // SAFETY: same controlled synchronous exception path as sleep. The caller
    // passes an opaque pointer whose lifetime is validated by the mailbox owner.
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
            );
        }
        op => panic!("task-call: unknown operation {op}"),
    }
}
