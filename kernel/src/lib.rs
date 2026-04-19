#![no_std]

pub mod arch_consts;
pub mod boot;
pub mod console;
mod dtb;
pub mod log;
pub mod memory;
pub mod panic;
pub mod sched;
pub mod time;

use bootinfo::BootInfo;

pub const TEST_PRIORITY: u8 = 10;
pub const TEST_RR_QUANTUM_MS: u64 = 10;
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
        crate::info!("bootinfo: dtb=present size={} bytes", boot.dtb_size);
    } else {
        crate::info!("bootinfo: dtb=absent");
    }

    if memory::init(boot).is_err() {
        fatal("memory: failed to initialize physical memory subsystem");
    }

    if sched::bootstrap(idle_task, &DEMO_TASKS, TEST_RR_QUANTUM_MS).is_err() {
        fatal("sched: failed to bootstrap scheduler");
    }

    crate::info!("sched: irq-return preemptive switching initialized");

    // Enters the running task through architecture trap-frame restore and never returns.
    sched::enter_running_task()
}

fn idle_task() -> ! {
    let mut last_log_ms = 0u64;
    loop {
        let now_ms = time::uptime_ms();
        if now_ms.wrapping_sub(last_log_ms) >= 5_000 {
            last_log_ms = now_ms;
            crate::trace!("idle: alive at {now_ms} ms");
        }
        core::hint::spin_loop();
    }
}

fn test_task_1() -> ! {
    let mut last_log_ms = 0u64;
    loop {
        let now_ms = time::uptime_ms();
        if now_ms.wrapping_sub(last_log_ms) >= 1_000 {
            last_log_ms = now_ms;
            crate::kprintln!("task1: cpu-bound at {now_ms} ms");
        }
        core::hint::spin_loop();
    }
}

fn test_task_2() -> ! {
    let mut cycle = 0u64;
    let sleep_ms = 2_000;
    loop {
        cycle = cycle.wrapping_add(1);

        crate::kprintln!("task2: sleeping for {sleep_ms} ms, cycle {cycle}");
        sched::msleep(sleep_ms);
        crate::kprintln!("task2: woke, cycle {cycle}");
    }
}

fn test_task_3() -> ! {
    let mut cycle = 0u64;
    let sleep_ms = 6_000;
    loop {
        cycle = cycle.wrapping_add(1);

        crate::kprintln!("task3: sleeping for {sleep_ms} ms, cycle {cycle}");
        sched::msleep(sleep_ms);
        crate::kprintln!("task3: woke, cycle {cycle}");
    }
}

fn fatal(msg: &str) -> ! {
    crate::error!("{msg}");
    // SAFETY: kernel fatal path is terminal and should converge with panic behavior.
    unsafe { arch_hard_fault() }
}
