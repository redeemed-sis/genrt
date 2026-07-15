#![no_std]
// QEMU scenario features replace production roots with finite test
// coordinators, so production-only call graphs are intentionally unreachable
// in those artifacts. Production builds retain the normal dead-code lint.
#![cfg_attr(feature = "qemu-test", allow(dead_code))]

extern crate alloc;

pub mod arch;
pub mod boot;
mod config;
pub mod console;
mod dtb;
pub mod errno;
pub mod fs;
mod init;
pub mod ipc;
pub mod limits;
pub mod loader;
pub mod log;
pub mod memory;
pub mod panic;
pub mod process;
pub mod sched;
pub(crate) mod sync;
pub mod syscall;
pub mod task;
pub mod task_call;
#[cfg(feature = "qemu-test")]
mod test_support;
pub mod time;

use bootinfo::BootInfo;

#[cfg(not(any(feature = "qemu-test-kernel-runtime", feature = "qemu-test-user-fault")))]
const PRODUCTION_TASKS: [sched::StaticTask; 1] = [sched::StaticTask::new(
    crate::init::kernel_init_thread,
    sched::ThreadArg::empty(),
)];

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(boot: &'static BootInfo) -> ! {
    crate::info!("kernel_main entered");
    crate::info!("bootinfo: arch=aarch64");

    if boot.dtb_pa != 0 {
        crate::info!("bootinfo: dtb=present size={} bytes", boot.dtb_size);
    } else {
        crate::info!("bootinfo: dtb=absent");
    }

    if let Err(err) = memory::init(boot) {
        crate::error!("memory: init failed: {:?}", err);
        panic!("memory: failed to initialize physical memory subsystem");
    }

    log_bootstrap_stack_usage("after memory init");
    if let Err(err) = unsafe { memory::vm::switch_to_runtime_kernel_tables() } {
        crate::error!(
            "memory: failed to switch to runtime kernel page tables: {:?}",
            err
        );
        panic!("memory: failed to switch to runtime kernel page tables");
    }
    crate::info!("memory: switched to runtime kernel page tables; TTBR0 cleared");

    if let Err(err) = fs::initramfs::mount_from_loader_region() {
        crate::error!("initramfs: mount failed: {:?}", err);
        panic!("initramfs: failed to mount loader image");
    }

    #[cfg(feature = "qemu-test-kernel-runtime")]
    test_support::kernel_runtime::init();

    if sched::bootstrap(
        idle_task,
        sched::ThreadArg::empty(),
        static_tasks(),
        config::SCHED_RR_QUANTUM_MS,
        config::KERNEL_THREAD_CAPACITY,
    )
    .is_err()
    {
        panic!("sched: failed to bootstrap scheduler");
    }

    log_bootstrap_stack_usage("before first task");
    crate::info!("sched: irq-return preemptive switching initialized");
    // Enters the running task through architecture trap-frame restore and never returns.
    sched::enter_running_task()
}

fn static_tasks() -> &'static [sched::StaticTask] {
    #[cfg(feature = "qemu-test-kernel-runtime")]
    {
        &test_support::kernel_runtime::TASKS
    }
    #[cfg(feature = "qemu-test-user-fault")]
    {
        &test_support::user_fault::TASKS
    }
    #[cfg(not(any(feature = "qemu-test-kernel-runtime", feature = "qemu-test-user-fault")))]
    {
        &PRODUCTION_TASKS
    }
}

fn idle_task(_arg: sched::ThreadArg) -> usize {
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

fn log_bootstrap_stack_usage(stage: &str) {
    let usage = boot::bootstrap_stack_usage();
    crate::info!(
        "boot stack: stage={stage} used={}B unused={}B total={}B low=0x{:x}",
        usage.used_bytes,
        usage.unused_bytes,
        usage.total_bytes,
        usage.lowest_used_addr
    );
}
