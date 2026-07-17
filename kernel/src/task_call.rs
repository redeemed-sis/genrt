const TASK_CALL_SLEEP_UNTIL: u64 = 1;
const TASK_CALL_MAILBOX_WAIT: u64 = 2;
const TASK_CALL_THREAD_EXIT: u64 = 3;
const TASK_CALL_THREAD_JOIN: u64 = 4;
const TASK_CALL_PROCESS_JOIN: u64 = 5;
const TASK_CALL_PREEMPT_CHECKPOINT: u64 = 6;
#[cfg(feature = "qemu-test-kernel-runtime")]
const TASK_CALL_TEST_WAIT: u64 = 7;

use crate::{
    arch::ActiveContext,
    process::ProcessId,
    sched::{WaitCause, WaitToken},
    task::ThreadId,
};

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
    process_join: TaskCallProcessJoin,
    preempt_checkpoint: TaskCallPreemptCheckpoint,
    #[cfg(feature = "qemu-test-kernel-runtime")]
    test_wait: TaskCallTestWait,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct TaskCallSleepUntil {
    deadline: u64,
    output: *mut TaskCallWaitOutput,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct TaskCallMailboxWait {
    mailbox: *const core::ffi::c_void,
    wait_kind: u64,
    timeout_enabled: u64,
    deadline: u64,
    output: *mut TaskCallWaitOutput,
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
    output: *mut TaskCallWaitOutput,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct TaskCallProcessJoin {
    index: usize,
    generation: u32,
    output: *mut TaskCallWaitOutput,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct TaskCallPreemptCheckpoint;

#[cfg(feature = "qemu-test-kernel-runtime")]
#[repr(C)]
#[derive(Copy, Clone)]
struct TaskCallTestWait {
    mode: u64,
    output: *mut TaskCallWaitOutput,
}

impl TaskCallRequest {
    fn sleep_until(deadline: u64, output: &mut TaskCallWaitOutput) -> Self {
        Self {
            op: TASK_CALL_SLEEP_UNTIL,
            args: TaskCallArgs {
                sleep_until: TaskCallSleepUntil { deadline, output },
            },
        }
    }

    fn mailbox_wait(
        mailbox: *const core::ffi::c_void,
        wait_kind: u64,
        timeout_deadline: Option<u64>,
        output: &mut TaskCallWaitOutput,
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
                    output,
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

    fn thread_join(id: ThreadId, output: &mut TaskCallWaitOutput) -> Self {
        Self {
            op: TASK_CALL_THREAD_JOIN,
            args: TaskCallArgs {
                thread_join: TaskCallThreadJoin {
                    index: id.index(),
                    generation: id.generation(),
                    output,
                },
            },
        }
    }

    fn process_join(id: ProcessId, output: &mut TaskCallWaitOutput) -> Self {
        Self {
            op: TASK_CALL_PROCESS_JOIN,
            args: TaskCallArgs {
                process_join: TaskCallProcessJoin {
                    index: id.index(),
                    generation: id.generation(),
                    output,
                },
            },
        }
    }

    fn preempt_checkpoint() -> Self {
        Self {
            op: TASK_CALL_PREEMPT_CHECKPOINT,
            args: TaskCallArgs {
                preempt_checkpoint: TaskCallPreemptCheckpoint,
            },
        }
    }

    #[cfg(feature = "qemu-test-kernel-runtime")]
    fn test_wait(mode: u64, output: &mut TaskCallWaitOutput) -> Self {
        Self {
            op: TASK_CALL_TEST_WAIT,
            args: TaskCallArgs {
                test_wait: TaskCallTestWait { mode, output },
            },
        }
    }
}

/// Exact completion returned by a private blocking task call.
///
/// A completion carries the original token only for owner-side loser cleanup;
/// the scheduler metadata has already been consumed by the time this value is
/// returned.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct WaitCallCompletion {
    token: WaitToken,
    cause: WaitCause,
}

impl WaitCallCompletion {
    /// Return the exact registration token published by the operation.
    ///
    /// # Returns
    ///
    /// Returns a copy without changing scheduler state, allocating, blocking,
    /// or touching IRQ state.
    pub(crate) const fn token(self) -> WaitToken {
        self.token
    }

    /// Return the first-wins completion cause.
    ///
    /// # Returns
    ///
    /// Returns the cause without changing scheduler state, allocating,
    /// blocking, or touching IRQ state.
    pub(crate) const fn cause(self) -> WaitCause {
        self.cause
    }
}

/// Mutable private task-call output retaining one exact wait token.
///
/// The request owner creates this on its stack, the controlled EL1 exception
/// writes it before any handoff, and the wrapper consumes it after resume.
/// It is private to the EL1 task-call ABI and does not change the EL0 syscall
/// ABI.
pub(crate) struct TaskCallWaitOutput {
    token: Option<WaitToken>,
    early: Option<WaitCause>,
}

impl TaskCallWaitOutput {
    /// Construct empty output for one private task-call request.
    ///
    /// # Returns
    ///
    /// Returns stack-owned empty output without allocation or IRQ changes.
    pub(crate) const fn new() -> Self {
        Self {
            token: None,
            early: None,
        }
    }

    /// Record the token before the external owner publishes it.
    ///
    /// # Arguments
    ///
    /// * `token` - Exact prepared token stored by the owner and scheduler.
    ///
    /// # Returns
    ///
    /// Returns after retaining `token`; this is bounded and allocation-free.
    pub(crate) fn record_token(&mut self, token: WaitToken) {
        self.token = Some(token);
    }

    /// Record an early completion consumed by commit without a handoff.
    ///
    /// # Arguments
    ///
    /// * `cause` - First-wins completion cause observed during commit.
    ///
    /// # Returns
    ///
    /// Returns after retaining `cause`; this is bounded and allocation-free.
    pub(crate) fn record_early(&mut self, cause: WaitCause) {
        self.early = Some(cause);
    }

    /// Consume the exact completion after the task-call path returns.
    ///
    /// # Returns
    ///
    /// Returns `None` when the owner skipped registration. Otherwise returns
    /// the early cause or calls exact scheduler finish; neither path allocates.
    ///
    /// # Panics
    ///
    /// Panics if a resumed task-call token is not completed by its owner.
    pub(crate) fn take_completion(&mut self) -> Option<WaitCallCompletion> {
        let token = self.token.take()?;
        let cause = match self.early.take() {
            Some(cause) => cause,
            None => crate::sched::finish_wait(token)
                .unwrap_or_else(|error| panic!("task-call: exact wait finish failed: {error:?}")),
        };
        Some(WaitCallCompletion { token, cause })
    }
}

/// Enter the private EL1 scheduler checkpoint task call.
///
/// The checkpoint services an already pending request only when preemption is
/// enabled. It is bounded and allocation-free; it may replace the live task
/// context but does not itself create a reschedule request.
///
/// # Returns
///
/// Returns after the private EL1 task call retains or replaces the task's live
/// context. It leaves a request pending when preemption is disabled.
pub(crate) fn preempt_checkpoint() {
    let request = TaskCallRequest::preempt_checkpoint();
    // SAFETY: the request has no borrowed payload and is consumed synchronously
    // by the controlled current-EL task-call path before this function returns.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
}

pub(crate) fn sleep_until_counter(deadline: u64) {
    let mut output = TaskCallWaitOutput::new();
    let request = TaskCallRequest::sleep_until(deadline, &mut output);
    // SAFETY: `arch_task_call()` enters a controlled synchronous exception path.
    // The architecture saves the current task frame and routes the typed request
    // back into `on_arch_task_call()`. The request lives on the current task's
    // stack and is consumed synchronously before this function can return.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
    let _ = output.take_completion();
}

/// Enter a private task call that waits on one mailbox condition.
///
/// The mailbox owner publishes an exact wait token while the controlled
/// exception keeps local IRQs masked. The task call may block, but performs no
/// allocation in the scheduler path.
///
/// # Arguments
///
/// * `mailbox` - Opaque pointer to the live mailbox control lock that owns the
///   condition and waiter queue.
/// * `wait_kind` - Mailbox-private numeric selector for send or receive
///   availability.
///
/// # Returns
///
/// Returns `Some` with the exact token and first-wins cause after a published
/// wait completes. Returns `None` when the mailbox condition no longer requires
/// registration at exception time.
///
/// # Safety
///
/// `mailbox` must remain valid for the entire synchronous task-call operation,
/// including any blocked interval, and must point to the mailbox control type
/// expected by [`crate::ipc::on_mailbox_wait_sync`].
pub(crate) fn mailbox_wait(
    mailbox: *const core::ffi::c_void,
    wait_kind: u64,
) -> Option<WaitCallCompletion> {
    let mut output = TaskCallWaitOutput::new();
    let request = TaskCallRequest::mailbox_wait(mailbox, wait_kind, None, &mut output);
    // SAFETY: same controlled synchronous exception path as sleep. The caller
    // passes an opaque pointer whose lifetime is validated by the mailbox owner.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
    output.take_completion()
}

/// Enter a private timed task call waiting on one mailbox condition.
///
/// This has the same owner and allocation contract as [`mailbox_wait`], and
/// additionally publishes one exact time-owned deadline for the wait token.
///
/// # Arguments
///
/// * `mailbox` - Opaque pointer to the live mailbox control lock that owns the
///   condition and waiter queue.
/// * `wait_kind` - Mailbox-private numeric selector for send or receive
///   availability.
/// * `deadline` - Absolute architecture counter at which timeout may complete
///   the exact wait.
///
/// # Returns
///
/// Returns `Some` with the exact token and either notified or timeout cause
/// after registration. Returns `None` when no wait is required at exception
/// time.
///
/// # Safety
///
/// `mailbox` must satisfy the validity and lifetime contract documented by
/// [`mailbox_wait`].
pub(crate) fn mailbox_wait_until_counter(
    mailbox: *const core::ffi::c_void,
    wait_kind: u64,
    deadline: u64,
) -> Option<WaitCallCompletion> {
    let mut output = TaskCallWaitOutput::new();
    let request = TaskCallRequest::mailbox_wait(mailbox, wait_kind, Some(deadline), &mut output);
    // SAFETY: same controlled synchronous exception path as sleep. The timeout
    // deadline is an absolute architecture counter value consumed synchronously.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
    output.take_completion()
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
    let mut output = TaskCallWaitOutput::new();
    let request = TaskCallRequest::thread_join(id, &mut output);
    // SAFETY: the join request is consumed synchronously by the controlled SVC
    // path before this stack frame can go away or the caller blocks.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
    let _ = output.take_completion();
}

pub(crate) fn process_join(id: ProcessId) {
    let mut output = TaskCallWaitOutput::new();
    let request = TaskCallRequest::process_join(id, &mut output);
    // SAFETY: process join is consumed synchronously by the controlled SVC path.
    // If the process is still running, the current kernel thread may block and
    // resume after the process stores a terminal exit status.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
    let _ = output.take_completion();
}

/// Run one QEMU-test-only exact-token wait scenario.
///
/// This private EL1 test seam is compiled only into the finite kernel-runtime
/// fixture. It neither changes the EL0 syscall ABI nor appears in production
/// artifacts.
///
/// # Arguments
///
/// * `mode` - Bounded coordinator-selected event/timeout ordering.
///
/// # Returns
///
/// Returns the exact first-wins completion from the test scenario.
///
/// # Panics
///
/// Panics when the controlled test registration does not complete.
#[cfg(feature = "qemu-test-kernel-runtime")]
pub(crate) fn test_wait(mode: u64) -> WaitCallCompletion {
    let mut output = TaskCallWaitOutput::new();
    let request = TaskCallRequest::test_wait(mode, &mut output);
    // SAFETY: same synchronous controlled task-call lifetime as other waits.
    unsafe { arch_task_call(&request as *const TaskCallRequest as *const core::ffi::c_void) }
    output
        .take_completion()
        .unwrap_or_else(|| panic!("task-call: test wait skipped registration"))
}

/// Dispatch one controlled EL1 task call against the live exception context.
///
/// Requests are consumed synchronously from the calling task's stack. Blocking
/// operations hand the context to the scheduler without allocation; they may
/// resume only after the owning subsystem wakes the task.
///
/// # Arguments
///
/// * `context` - Exclusive live current-EL exception context used for scheduler
///   save/replace handoff.
/// * `request` - Non-null pointer to the private task-call request created by
///   this module on the caller's stack.
///
/// # Returns
///
/// Returns after a non-blocking task call or after a blocked task is later
/// resumed. Thread exit replaces the live context and does not resume the
/// exiting task.
///
/// # Panics
///
/// Panics for a null request, an unknown operation tag, or malformed scheduler
/// state reached by the selected operation.
pub fn on_arch_task_call(context: &mut ActiveContext<'_>, request: *const core::ffi::c_void) {
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
            // SAFETY: the wrapper stores stack-owned output in this private
            // synchronous request until the task call returns after any resume.
            let output = unsafe { args.output.as_mut() }
                .unwrap_or_else(|| panic!("task-call: null sleep wait output"));
            crate::sched::on_sleep_sync(context, args.deadline, output);
        }
        TASK_CALL_MAILBOX_WAIT => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.mailbox_wait };
            crate::ipc::on_mailbox_wait_sync(
                context,
                args.mailbox.cast_mut(),
                args.wait_kind,
                if args.timeout_enabled != 0 {
                    Some(args.deadline)
                } else {
                    None
                },
                // SAFETY: see the sleep output contract above.
                unsafe { args.output.as_mut() }
                    .unwrap_or_else(|| panic!("task-call: null mailbox wait output")),
            );
        }
        TASK_CALL_THREAD_EXIT => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.thread_exit };
            crate::sched::on_thread_exit_sync(context, args.code);
        }
        TASK_CALL_THREAD_JOIN => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.thread_join };
            // SAFETY: see the sleep output contract above.
            let output = unsafe { args.output.as_mut() }
                .unwrap_or_else(|| panic!("task-call: null thread wait output"));
            crate::sched::on_thread_join_sync(
                context,
                ThreadId::new(args.index, args.generation),
                output,
            );
        }
        TASK_CALL_PROCESS_JOIN => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.process_join };
            crate::process::on_process_join_sync(
                context,
                ProcessId::new(args.index, args.generation),
                // SAFETY: see the sleep output contract above.
                unsafe { args.output.as_mut() }
                    .unwrap_or_else(|| panic!("task-call: null process wait output")),
            );
        }
        TASK_CALL_PREEMPT_CHECKPOINT => {
            // SAFETY: the `op` tag selects this payload variant, whose unit
            // representation has no data to inspect.
            let _ = unsafe { request.args.preempt_checkpoint };
            crate::sched::on_preempt_checkpoint(context);
        }
        #[cfg(feature = "qemu-test-kernel-runtime")]
        TASK_CALL_TEST_WAIT => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.test_wait };
            // SAFETY: see the stack-owned output contract above.
            let output = unsafe { args.output.as_mut() }
                .unwrap_or_else(|| panic!("task-call: null test wait output"));
            crate::sched::on_test_wait_sync(context, args.mode, output);
        }
        op => panic!("task-call: unknown operation {op}"),
    }
}
