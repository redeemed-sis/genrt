use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{ipc::Mailbox, sched};

const DEMO_MAILBOX_CAPACITY: usize = 1;
type DemoMessage = usize;

pub(crate) const TASKS: [sched::StaticTask; 3] = [
    sched::StaticTask::new(crate::TEST_PRIORITY, consumer_task),
    sched::StaticTask::new(crate::TEST_PRIORITY, producer_task),
    sched::StaticTask::new(crate::TEST_PRIORITY, sleeper_task),
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

fn consumer_task() -> ! {
    loop {
        crate::debug!("consumer: recv wait");
        let msg = demo_mailbox().recv();
        crate::info!("consumer: recv {msg}");
        sched::msleep(2_000);
    }
}

fn producer_task() -> ! {
    let mut msg: DemoMessage = 1;
    loop {
        demo_mailbox().send(msg);
        crate::info!("producer: send {msg}");
        msg = msg.wrapping_add(1);
        sched::msleep(500);
    }
}

fn sleeper_task() -> ! {
    let mut cycle = 0u64;
    let sleep_ms = 6_000;
    loop {
        cycle = cycle.wrapping_add(1);

        crate::debug!("sleeper: sleeping for {sleep_ms} ms, cycle {cycle}");
        sched::msleep(sleep_ms);
        crate::debug!("sleeper: woke, cycle {cycle}");
    }
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
