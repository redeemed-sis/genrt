#![no_std]

use core::cell::UnsafeCell;

pub mod boot;
pub mod console;
pub mod debug;
pub mod panic;
pub mod sched;
pub mod time;

use bootinfo::BootInfo;

pub const TEST_TASK_ID: sched::TaskId = 1;
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

    scheduler_mut().init();
    if scheduler_mut()
        .add_for_scheduling(TEST_TASK_ID, TEST_PRIORITY)
        .is_err()
    {
        console::puts("[genrt] sched: failed to add test task\r\n");
    }
    console::puts("[genrt] sched: fixed-priority skeleton initialized\r\n");

    loop {
        core::hint::spin_loop();
    }
}

#[inline(always)]
pub fn on_tick_interrupt() {
    time::on_tick_interrupt();
    scheduler_mut().on_tick();
}

#[inline(always)]
fn scheduler_mut() -> &'static mut sched::Scheduler {
    // SAFETY: Access is single-writer in current single-core bring-up model.
    unsafe { &mut *SCHEDULER.0.get() }
}
