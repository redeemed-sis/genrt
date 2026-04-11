#![no_std]

use core::cell::UnsafeCell;

pub mod arch_consts;
pub mod boot;
pub mod console;
pub mod debug;
pub mod panic;
pub mod sched;
pub mod time;

use bootinfo::BootInfo;

pub const TEST_PRIORITY: u8 = 10;

struct SchedulerCell(UnsafeCell<sched::Scheduler>);

// SAFETY: genrt currently runs scheduler mutations only on a single core.
unsafe impl Sync for SchedulerCell {}

static SCHEDULER: SchedulerCell = SchedulerCell(UnsafeCell::new(sched::Scheduler::new()));

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(boot: &'static BootInfo) -> ! {
    console::puts("[genrt] kernel_main: entered\r\n");
    console::puts("[genrt] bootinfo:\r\n");
    console::puts("  arch=aarch64\r\n");

    if boot.dtb_pa != 0 {
        console::puts("  dtb=present\r\n");
    } else {
        console::puts("  dtb=absent\r\n");
    }

    let sched = scheduler_mut();

    if sched.bootstrap(idle_task).is_err() {
        fatal("[genrt] sched: failed to bootstrap scheduler\r\n");
    }

    if sched.add_task(TEST_PRIORITY, test_task_1).is_err() {
        fatal("[genrt] sched: failed to add test task\r\n");
    }

    if sched.add_task(TEST_PRIORITY, test_task_2).is_err() {
        fatal("[genrt] sched: failed to add test task\r\n");
    }

    console::puts("[genrt] sched: irq-return preemptive switching initialized\r\n");

    // Enters the running task through architecture trap-frame restore and never returns.
    sched.enter_running_task()
}

#[unsafe(no_mangle)]
pub extern "C" fn on_tick_interrupt(frame_words: *mut u64) {
    if frame_words.is_null() {
        return;
    }

    time::on_tick_interrupt();
    scheduler_mut().preempt_on_tick(frame_words);
}

fn idle_task() -> ! {
    let mut last_log_tick = 0u64;
    loop {
        let now = time::ticks();
        if now.wrapping_sub(last_log_tick) >= 500 {
            last_log_tick = now;
            console::puts("[idle] alive\r\n");
        }
        core::hint::spin_loop();
    }
}

fn test_task_1() -> ! {
    let mut last_log_tick = 0u64;
    loop {
        let now = time::ticks();
        if now.wrapping_sub(last_log_tick) >= 500 {
            last_log_tick = now;
            console::puts("[task1] alive\r\n");
        }
        core::hint::spin_loop();
    }
}

fn test_task_2() -> ! {
    let mut last_log_tick = 0u64;
    scheduler_mut()
        .add_task(TEST_PRIORITY, test_task_3)
        .unwrap();
    loop {
        let now = time::ticks();
        if now.wrapping_sub(last_log_tick) >= 500 {
            last_log_tick = now;
            console::puts("[task2] alive\r\n");
        }
        core::hint::spin_loop();
    }
}

fn test_task_3() -> ! {
    let mut last_log_tick = 0u64;
    loop {
        let now = time::ticks();
        if now.wrapping_sub(last_log_tick) >= 500 {
            last_log_tick = now;
            console::puts("[task3] from another task is alive\r\n");
        }
        core::hint::spin_loop();
    }
}

fn fatal(msg: &str) -> ! {
    console::puts(msg);
    loop {
        core::hint::spin_loop();
    }
}

#[inline(always)]
fn scheduler_mut() -> &'static mut sched::Scheduler {
    // SAFETY: Access is single-writer in current single-core bring-up model.
    unsafe { &mut *SCHEDULER.0.get() }
}
