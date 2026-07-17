//! Controlled synchronous entry into scheduler operations.
//!
//! Thread-context wrappers build a stack-owned request and invoke the private
//! architecture `arch_sched_call` hook. The AArch64 exception layer returns
//! the request to [`on_arch_sched_call`], which dispatches only bounded
//! scheduler handoff operations. Operation numbers and request layout form an
//! internal kernel/architecture ABI; this module is not an EL0 syscall API.

const SCHED_CALL_SLEEP_UNTIL: u64 = 1;
const SCHED_CALL_MAILBOX_WAIT: u64 = 2;
const SCHED_CALL_THREAD_EXIT: u64 = 3;
const SCHED_CALL_THREAD_JOIN: u64 = 4;
// ABI operation 5 remains intentionally unassigned after process joins moved
// to ordinary thread joins; do not reuse this numeric gap.
const SCHED_CALL_PREEMPT_CHECKPOINT: u64 = 6;
#[cfg(feature = "qemu-test-kernel-runtime")]
const SCHED_CALL_TEST_WAIT: u64 = 7;

use crate::arch::ActiveContext;

use super::{ThreadId, WaitCause, WaitToken};

unsafe extern "C" {
    fn arch_sched_call(request: *const core::ffi::c_void);
}

#[repr(C)]
struct SchedCallRequest {
    op: u64,
    args: SchedCallArgs,
}

#[repr(C)]
union SchedCallArgs {
    sleep_until: SchedCallSleepUntil,
    mailbox_wait: SchedCallMailboxWait,
    thread_exit: SchedCallThreadExit,
    thread_join: SchedCallThreadJoin,
    preempt_checkpoint: SchedCallPreemptCheckpoint,
    #[cfg(feature = "qemu-test-kernel-runtime")]
    test_wait: SchedCallTestWait,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SchedCallSleepUntil {
    deadline: u64,
    output: *mut SchedCallWaitOutput,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SchedCallMailboxWait {
    mailbox: *const core::ffi::c_void,
    wait_kind: u64,
    timeout_enabled: u64,
    deadline: u64,
    output: *mut SchedCallWaitOutput,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SchedCallThreadExit {
    code: usize,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SchedCallThreadJoin {
    index: usize,
    generation: u32,
    output: *mut SchedCallWaitOutput,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct SchedCallPreemptCheckpoint;

#[cfg(feature = "qemu-test-kernel-runtime")]
#[repr(C)]
#[derive(Copy, Clone)]
struct SchedCallTestWait {
    mode: u64,
    output: *mut SchedCallWaitOutput,
}

impl SchedCallRequest {
    fn sleep_until(deadline: u64, output: &mut SchedCallWaitOutput) -> Self {
        Self {
            op: SCHED_CALL_SLEEP_UNTIL,
            args: SchedCallArgs {
                sleep_until: SchedCallSleepUntil { deadline, output },
            },
        }
    }

    fn mailbox_wait(
        mailbox: *const core::ffi::c_void,
        wait_kind: u64,
        timeout_deadline: Option<u64>,
        output: &mut SchedCallWaitOutput,
    ) -> Self {
        let (timeout_enabled, deadline) = match timeout_deadline {
            Some(deadline) => (1, deadline),
            None => (0, 0),
        };

        Self {
            op: SCHED_CALL_MAILBOX_WAIT,
            args: SchedCallArgs {
                mailbox_wait: SchedCallMailboxWait {
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
            op: SCHED_CALL_THREAD_EXIT,
            args: SchedCallArgs {
                thread_exit: SchedCallThreadExit { code },
            },
        }
    }

    fn thread_join(id: ThreadId, output: &mut SchedCallWaitOutput) -> Self {
        Self {
            op: SCHED_CALL_THREAD_JOIN,
            args: SchedCallArgs {
                thread_join: SchedCallThreadJoin {
                    index: id.index(),
                    generation: id.generation(),
                    output,
                },
            },
        }
    }

    fn preempt_checkpoint() -> Self {
        Self {
            op: SCHED_CALL_PREEMPT_CHECKPOINT,
            args: SchedCallArgs {
                preempt_checkpoint: SchedCallPreemptCheckpoint,
            },
        }
    }

    #[cfg(feature = "qemu-test-kernel-runtime")]
    fn test_wait(mode: u64, output: &mut SchedCallWaitOutput) -> Self {
        Self {
            op: SCHED_CALL_TEST_WAIT,
            args: SchedCallArgs {
                test_wait: SchedCallTestWait { mode, output },
            },
        }
    }
}

/// Exact completion returned by a private blocking sched call.
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

/// Mutable private sched-call output retaining one exact wait token.
///
/// The request owner creates this on its stack, the controlled EL1 exception
/// writes it before any handoff, and the wrapper consumes it after resume.
/// It is private to the EL1 sched-call ABI and does not change the EL0 syscall
/// ABI.
pub(crate) struct SchedCallWaitOutput {
    token: Option<WaitToken>,
    early: Option<WaitCause>,
}

impl SchedCallWaitOutput {
    /// Construct empty output for one private sched-call request.
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

    /// Consume the exact completion after the sched-call path returns.
    ///
    /// # Returns
    ///
    /// Returns `None` when the owner skipped registration. Otherwise returns
    /// the early cause or calls exact scheduler finish; neither path allocates.
    ///
    /// # Panics
    ///
    /// Panics if a resumed sched-call token is not completed by its owner.
    pub(crate) fn take_completion(&mut self) -> Option<WaitCallCompletion> {
        let token = self.token.take()?;
        let cause = match self.early.take() {
            Some(cause) => cause,
            None => crate::sched::finish_wait(token)
                .unwrap_or_else(|error| panic!("sched-call: exact wait finish failed: {error:?}")),
        };
        Some(WaitCallCompletion { token, cause })
    }
}

/// Enter the private EL1 scheduler checkpoint sched call.
///
/// The checkpoint services an already pending request only when preemption is
/// enabled. It is bounded and allocation-free; it may replace the live thread
/// context but does not itself create a reschedule request.
///
/// # Returns
///
/// Returns after the private EL1 sched call retains or replaces the thread's live
/// context. It leaves a request pending when preemption is disabled.
pub(crate) fn preempt_checkpoint() {
    let request = SchedCallRequest::preempt_checkpoint();
    // SAFETY: the request has no borrowed payload and is consumed synchronously
    // by the controlled current-EL sched-call path before this function returns.
    unsafe { arch_sched_call(&request as *const SchedCallRequest as *const core::ffi::c_void) }
}

pub(crate) fn sleep_until_counter(deadline: u64) {
    let mut output = SchedCallWaitOutput::new();
    let request = SchedCallRequest::sleep_until(deadline, &mut output);
    // SAFETY: `arch_sched_call()` enters a controlled synchronous exception path.
    // The architecture saves the current thread frame and routes the typed request
    // back into `on_arch_sched_call()`. The request lives on the current thread's
    // stack and is consumed synchronously before this function can return.
    unsafe { arch_sched_call(&request as *const SchedCallRequest as *const core::ffi::c_void) }
    let _ = output.take_completion();
}

/// Enter a private sched call that waits on one mailbox condition.
///
/// The mailbox owner publishes an exact wait token while the controlled
/// exception keeps local IRQs masked. The sched call may block, but performs no
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
/// `mailbox` must remain valid for the entire synchronous sched-call operation,
/// including any blocked interval, and must point to the mailbox control type
/// expected by [`crate::ipc::on_mailbox_wait_sync`].
pub(crate) fn mailbox_wait(
    mailbox: *const core::ffi::c_void,
    wait_kind: u64,
) -> Option<WaitCallCompletion> {
    let mut output = SchedCallWaitOutput::new();
    let request = SchedCallRequest::mailbox_wait(mailbox, wait_kind, None, &mut output);
    // SAFETY: same controlled synchronous exception path as sleep. The caller
    // passes an opaque pointer whose lifetime is validated by the mailbox owner.
    unsafe { arch_sched_call(&request as *const SchedCallRequest as *const core::ffi::c_void) }
    output.take_completion()
}

/// Enter a private timed sched call waiting on one mailbox condition.
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
    let mut output = SchedCallWaitOutput::new();
    let request = SchedCallRequest::mailbox_wait(mailbox, wait_kind, Some(deadline), &mut output);
    // SAFETY: same controlled synchronous exception path as sleep. The timeout
    // deadline is an absolute architecture counter value consumed synchronously.
    unsafe { arch_sched_call(&request as *const SchedCallRequest as *const core::ffi::c_void) }
    output.take_completion()
}

pub(crate) fn thread_exit(code: usize) -> ! {
    let request = SchedCallRequest::thread_exit(code);
    // SAFETY: thread exit uses the same controlled synchronous exception path as
    // other scheduler sched calls. The scheduler replaces the active frame with a
    // different runnable thread, so this call must not return to the exiting one.
    unsafe { arch_sched_call(&request as *const SchedCallRequest as *const core::ffi::c_void) }
    panic!("sched-call: thread_exit returned to exiting thread");
}

pub(crate) fn thread_join(id: ThreadId) {
    let mut output = SchedCallWaitOutput::new();
    let request = SchedCallRequest::thread_join(id, &mut output);
    // SAFETY: the join request is consumed synchronously by the controlled SVC
    // path before this stack frame can go away or the caller blocks.
    unsafe { arch_sched_call(&request as *const SchedCallRequest as *const core::ffi::c_void) }
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
    let mut output = SchedCallWaitOutput::new();
    let request = SchedCallRequest::test_wait(mode, &mut output);
    // SAFETY: same synchronous controlled sched-call lifetime as other waits.
    unsafe { arch_sched_call(&request as *const SchedCallRequest as *const core::ffi::c_void) }
    output
        .take_completion()
        .unwrap_or_else(|| panic!("sched-call: test wait skipped registration"))
}

/// Dispatch one controlled EL1 sched call against the live exception context.
///
/// Requests are consumed synchronously from the calling thread's stack. Blocking
/// operations hand the context to the scheduler without allocation; they may
/// resume only after the owning subsystem wakes the thread.
///
/// # Arguments
///
/// * `context` - Exclusive live current-EL exception context used for scheduler
///   save/replace handoff.
/// * `request` - Non-null pointer to the private sched-call request created by
///   this module on the caller's stack.
///
/// # Returns
///
/// Returns after a non-blocking sched call or after a blocked thread is later
/// resumed. Thread exit replaces the live context and does not resume the
/// exiting thread.
///
/// # Panics
///
/// Panics for a null request, an unknown operation tag, or malformed scheduler
/// state reached by the selected operation.
pub fn on_arch_sched_call(context: &mut ActiveContext<'_>, request: *const core::ffi::c_void) {
    if request.is_null() {
        panic!("sched-call: null request");
    }

    // SAFETY: `arch_sched_call()` passes a pointer to a live request on the
    // current thread stack. The exception dispatcher consumes it synchronously
    // before returning or switching away from the thread.
    let request = unsafe { &*request.cast::<SchedCallRequest>() };

    match request.op {
        SCHED_CALL_SLEEP_UNTIL => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.sleep_until };
            // SAFETY: the wrapper stores stack-owned output in this private
            // synchronous request until the sched call returns after any resume.
            let output = unsafe { args.output.as_mut() }
                .unwrap_or_else(|| panic!("sched-call: null sleep wait output"));
            crate::sched::on_sleep_sync(context, args.deadline, output);
        }
        SCHED_CALL_MAILBOX_WAIT => {
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
                    .unwrap_or_else(|| panic!("sched-call: null mailbox wait output")),
            );
        }
        SCHED_CALL_THREAD_EXIT => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.thread_exit };
            crate::sched::on_thread_exit_sync(context, args.code);
        }
        SCHED_CALL_THREAD_JOIN => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.thread_join };
            // SAFETY: see the sleep output contract above.
            let output = unsafe { args.output.as_mut() }
                .unwrap_or_else(|| panic!("sched-call: null thread wait output"));
            crate::sched::on_thread_join_sync(
                context,
                ThreadId::new(args.index, args.generation),
                output,
            );
        }
        SCHED_CALL_PREEMPT_CHECKPOINT => {
            // SAFETY: the `op` tag selects this payload variant, whose unit
            // representation has no data to inspect.
            let _ = unsafe { request.args.preempt_checkpoint };
            crate::sched::on_preempt_checkpoint(context);
        }
        #[cfg(feature = "qemu-test-kernel-runtime")]
        SCHED_CALL_TEST_WAIT => {
            // SAFETY: the `op` tag selects this payload variant.
            let args = unsafe { request.args.test_wait };
            // SAFETY: see the stack-owned output contract above.
            let output = unsafe { args.output.as_mut() }
                .unwrap_or_else(|| panic!("sched-call: null test wait output"));
            crate::sched::on_test_wait_sync(context, args.mode, output);
        }
        op => panic!("sched-call: unknown operation {op}"),
    }
}
