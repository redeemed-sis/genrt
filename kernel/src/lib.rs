#![no_std]

pub mod arch_consts;
pub mod boot;
pub mod console;
pub mod log;
pub mod panic;
pub mod sched;
pub mod time;

use bootinfo::BootInfo;

pub const TEST_PRIORITY: u8 = 10;
const DEMO_TASKS: [sched::StaticTask; 3] = [
    sched::StaticTask::new(TEST_PRIORITY, test_task_1),
    sched::StaticTask::new(TEST_PRIORITY, test_task_2),
    sched::StaticTask::new(TEST_PRIORITY, test_task_3),
];

unsafe extern "C" {
    fn arch_hard_fault() -> !;
}

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(boot: &'static BootInfo) -> ! {
    crate::info!("kernel_main entered");
    crate::info!("bootinfo: arch=aarch64");

    if boot.dtb_pa != 0 {
        crate::info!("bootinfo: dtb=present");
    } else {
        crate::info!("bootinfo: dtb=absent");
    }

    if sched::bootstrap(idle_task, &DEMO_TASKS).is_err() {
        fatal("sched: failed to bootstrap scheduler");
    }

    crate::info!("sched: irq-return preemptive switching initialized");

    // Enters the running task through architecture trap-frame restore and never returns.
    sched::enter_running_task()
}

fn idle_task() -> ! {
    let mut last_log_tick = 0u64;
    loop {
        let now = time::ticks();
        if now.wrapping_sub(last_log_tick) >= 500 {
            last_log_tick = now;
            crate::trace!("idle: alive");
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
            crate::kprintln!("task1: alive");
        }
        core::hint::spin_loop();
    }
}

fn test_task_2() -> ! {
    let mut last_log_tick = 0u64;
    loop {
        let now = time::ticks();
        if now.wrapping_sub(last_log_tick) >= 500 {
            last_log_tick = now;
            crate::kprintln!("task2: alive");
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
            crate::kprintln!("task3: from another task is alive");
        }
        core::hint::spin_loop();
    }
}

fn fatal(msg: &str) -> ! {
    crate::error!("{msg}");
    // SAFETY: kernel fatal path is terminal and should converge with panic behavior.
    unsafe { arch_hard_fault() }
}
