use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{
    ipc::{Mailbox, RecvTimeoutError},
    sched::{self, ThreadArg, ThreadAttrs},
};

const DEMO_MAILBOX_CAPACITY: usize = 1;
type DemoMessage = usize;

pub(crate) const TASKS: [sched::StaticTask; 5] = [
    sched::StaticTask::new(
        crate::TEST_PRIORITY,
        crate::init::kernel_init_thread,
        ThreadArg::empty(),
    ),
    sched::StaticTask::new(crate::TEST_PRIORITY, consumer_task, ThreadArg::empty()),
    sched::StaticTask::new(crate::TEST_PRIORITY, producer_task, ThreadArg::empty()),
    sched::StaticTask::new(crate::TEST_PRIORITY, sleeper_task, ThreadArg::empty()),
    sched::StaticTask::new(crate::TEST_PRIORITY, thread_parent_task, ThreadArg::empty()),
];

struct DemoMailboxCell {
    value: UnsafeCell<MaybeUninit<Mailbox<DemoMessage>>>,
    initialized: AtomicBool,
}

// SAFETY: the demo mailbox is initialized once during bootstrap before tasks
// start, and then only accessed through `Mailbox`'s internal IRQ-save lock.
unsafe impl Sync for DemoMailboxCell {}

static DEMO_MAILBOX: DemoMailboxCell = DemoMailboxCell {
    value: UnsafeCell::new(MaybeUninit::uninit()),
    initialized: AtomicBool::new(false),
};

pub(crate) fn init() {
    if DEMO_MAILBOX.initialized.load(Ordering::Acquire) {
        panic!("demo: mailbox already initialized");
    }

    let mailbox = Mailbox::with_capacity(DEMO_MAILBOX_CAPACITY, TASKS.len() + 1);
    // SAFETY: `init()` runs once during bootstrap before demo tasks can access
    // the mailbox. The initialized flag is published only after the write.
    unsafe {
        (*DEMO_MAILBOX.value.get()).write(mailbox);
    }
    DEMO_MAILBOX.initialized.store(true, Ordering::Release);
    crate::debug!(
        "demo: mailbox created capacity={} waiter_capacity={}",
        DEMO_MAILBOX_CAPACITY,
        TASKS.len() + 1
    );
}

fn consumer_task(_arg: ThreadArg) -> usize {
    loop {
        crate::debug!("consumer: recv_timeout success scenario wait=3000ms");
        match demo_mailbox().recv_timeout_ms(3_000) {
            Ok(msg) => crate::debug!("consumer: recv_timeout Ok({msg}) before deadline"),
            Err(RecvTimeoutError::Timeout) => {
                crate::warn!("consumer: expected message before timeout")
            }
        }

        crate::debug!("consumer: recv_timeout timeout scenario wait=300ms");
        match demo_mailbox().recv_timeout_ms(300) {
            Ok(msg) => crate::warn!("consumer: expected timeout but received {msg}"),
            Err(RecvTimeoutError::Timeout) => {
                crate::debug!("consumer: recv_timeout Err(Timeout)")
            }
        }

        sched::msleep(200);
    }
}

fn producer_task(_arg: ThreadArg) -> usize {
    let mut msg: DemoMessage = 1;
    loop {
        sched::msleep(500);
        demo_mailbox().send(msg);
        crate::debug!("producer: send {msg}");
        msg = msg.wrapping_add(1);
        sched::msleep(1_500);
    }
}

fn sleeper_task(_arg: ThreadArg) -> usize {
    let mut cycle = 0u64;
    let sleep_ms = 6_000;
    loop {
        cycle = cycle.wrapping_add(1);

        crate::debug!("sleeper: sleeping for {sleep_ms} ms, cycle {cycle}");
        sched::msleep(sleep_ms);
        crate::debug!("sleeper: woke, cycle {cycle}");
    }
}

fn thread_parent_task(_arg: ThreadArg) -> usize {
    let mut arg = 41usize;
    loop {
        match sched::thread_spawn(
            worker_thread,
            ThreadArg::from_usize(arg),
            ThreadAttrs::joinable(),
        ) {
            Ok(id) => {
                crate::debug!("thread: spawned worker id={id} arg={arg}");
                match sched::thread_join(id) {
                    Ok(code) => crate::debug!("parent: join returned code={code}"),
                    Err(err) => crate::warn!("parent: join failed: {err:?}"),
                }
            }
            Err(err) => crate::warn!("thread: spawn failed: {err:?}"),
        }

        arg = arg.wrapping_add(1);
        sched::msleep(5_000);
    }
}

fn worker_thread(arg: ThreadArg) -> usize {
    let arg = arg.as_usize();
    crate::debug!("worker: start arg={arg}");
    sched::msleep(250);
    let code = arg.wrapping_add(1);
    crate::debug!("worker: exit code={code}");
    code
}

fn demo_mailbox() -> &'static Mailbox<DemoMessage> {
    if !DEMO_MAILBOX.initialized.load(Ordering::Acquire) {
        panic!("demo: mailbox is not initialized");
    }

    // SAFETY: the initialized flag proves `init()` wrote the mailbox. Runtime
    // access returns only a shared reference; mailbox internals provide
    // synchronization for concurrent task access.
    unsafe { (&*DEMO_MAILBOX.value.get()).assume_init_ref() }
}
