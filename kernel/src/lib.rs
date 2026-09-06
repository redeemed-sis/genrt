#![no_std]
// QEMU scenario features replace production roots with finite test
// coordinators, so production-only call graphs are intentionally unreachable
// in those artifacts. Production builds retain the normal dead-code lint.
#![cfg_attr(feature = "qemu-test", allow(dead_code))]

extern crate alloc;
#[cfg(test)]
extern crate std;
#[cfg(test)]
mod test_arch_stubs;

pub mod arch;
pub mod boot;
mod config;
pub mod console;
pub mod cpu;
mod dtb;
pub mod errno;
pub mod fs;
mod init;
pub mod ipc;
pub mod loader;
pub mod log;
pub mod memory;
pub mod panic;
mod power;
pub mod process;
pub mod sched;
pub mod sync;
pub mod syscall;
#[cfg(feature = "qemu-test")]
mod test_support;
pub mod time;

use bootinfo::BootInfo;

#[cfg(not(any(
    feature = "qemu-test-kernel-runtime",
    feature = "qemu-test-user-fault",
    feature = "qemu-test-smp-boot"
)))]
const PRODUCTION_THREADS: [sched::StaticThread; 1] = [sched::StaticThread::new(
    crate::init::kernel_init_thread,
    sched::ThreadArg::empty(),
)];

#[unsafe(no_mangle)]
/// Initialize global generic kernel state on logical CPU0 and enter scheduling.
///
/// The boot CPU registers itself, validates the platform CPU count, initializes
/// memory and initramfs, publishes bounded scheduler and per-CPU time state,
/// starts architecture-initialized secondary scheduler contexts, waits for each
/// to become online, and then enters CPU0's selected scheduler context.
/// CPU0-local interrupt setup is complete before the architecture calls this
/// entry.
///
/// # Arguments
///
/// * `boot` - Permanently resident immutable boot information published by the
///   architecture layer.
///
/// # Returns
///
/// This function never returns after entering the first scheduler context.
///
/// # Panics
///
/// Panics through the fatal path when CPU topology, memory, initramfs,
/// secondary startup, or scheduler bootstrap fails.
///
/// # Safety
///
/// The architecture must call this exactly once on the boot CPU with global
/// BSS and platform state initialized and with `boot` resident forever.
pub extern "C" fn kernel_main(boot: &'static BootInfo) -> ! {
    crate::info!("kernel_main entered");
    crate::info!("bootinfo: arch=aarch64");

    let boot_cpu = cpu::register_boot_cpu()
        .unwrap_or_else(|err| panic!("cpu: failed to register boot CPU: {err:?}"));
    crate::info!("cpu: registered boot CPU{}", boot_cpu.index());
    cpu::configure_expected_count(boot.cpu_count as usize)
        .unwrap_or_else(|err| panic!("cpu: invalid boot topology: {err:?}"));
    crate::info!("cpu: platform expects {} CPU(s)", boot.cpu_count);

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
        idle_thread,
        sched::ThreadArg::empty(),
        static_threads(),
        config::SCHED_RR_QUANTUM_MS,
        config::KERNEL_THREAD_CAPACITY,
    )
    .is_err()
    {
        panic!("sched: failed to bootstrap scheduler");
    }
    time::init_cpu_states(boot.cpu_count as usize)
        .unwrap_or_else(|err| panic!("time: failed to initialize CPU states: {err:?}"));
    cpu::start_and_wait_scheduler_online_secondaries()
        .unwrap_or_else(|err| panic!("cpu: secondary startup failed: {err:?}"));

    log_bootstrap_stack_usage("before first thread");
    crate::info!("sched: irq-return preemptive switching initialized");
    // Enters the running thread through architecture trap-frame restore and never returns.
    sched::enter_running_thread()
}

fn static_threads() -> &'static [sched::StaticThread] {
    #[cfg(feature = "qemu-test-kernel-runtime")]
    {
        &test_support::kernel_runtime::THREADS
    }
    #[cfg(feature = "qemu-test-user-fault")]
    {
        &test_support::user_fault::THREADS
    }
    #[cfg(feature = "qemu-test-smp-boot")]
    {
        &test_support::smp_boot::THREADS
    }
    #[cfg(not(any(
        feature = "qemu-test-kernel-runtime",
        feature = "qemu-test-user-fault",
        feature = "qemu-test-smp-boot"
    )))]
    {
        &PRODUCTION_THREADS
    }
}

/// Register an architecture-ready secondary CPU and enter its scheduler.
///
/// Architecture code calls this entry only after completing all CPU-local MMU,
/// exception-vector, interrupt-controller, and timer setup with asynchronous
/// exceptions masked. This function then owns the complete generic startup
/// transaction: logical registration, scheduler initialization, and first
/// saved-context entry.
///
/// # Arguments
///
/// * `logical_index` - CPU0-selected logical slot carried through PSCI and used
///   by the architecture for the matching private bootstrap stack.
///
/// # Returns
///
/// This function never returns. On success, the saved idle context restores
/// the architecture IRQ state and leaves the bootstrap stack. On failure, it
/// publishes the startup failure and panics before any scheduler context is
/// made online.
///
/// # Panics
///
/// Panics when registration fails, the logical identity does not match
/// `logical_index`, the CPU already owns an online scheduler context, the
/// preallocated idle slot cannot be claimed, or saved-context entry fails.
///
/// The startup transaction is bounded and allocation-free before the terminal
/// panic path. IRQ must remain masked when architecture code calls it.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_secondary_main(logical_index: usize) -> ! {
    let result = cpu::register_secondary_cpu(logical_index).and_then(|id| {
        if id.index() != logical_index || sched::scheduler_online(id) {
            Err(cpu::CpuRegistrationError::UnexpectedCpu)
        } else if sched::initialize_current_cpu(idle_thread, sched::ThreadArg::empty()) {
            Ok(())
        } else {
            Err(cpu::CpuRegistrationError::SecondaryStartupFailed)
        }
    });

    if let Err(err) = result {
        cpu::record_secondary_startup_failure();
        panic!("cpu: secondary generic startup failed: {err:?}");
    }

    sched::enter_running_thread()
}

/// Publish failure of architecture-owned secondary CPU initialization.
///
/// # Returns
///
/// Returns after a bounded atomic publication visible to CPU0. The function
/// allocates nothing, does not alter IRQ state, and does not enter a scheduler
/// context.
///
#[unsafe(no_mangle)]
pub extern "C" fn kernel_fail_secondary_cpu_startup() {
    cpu::record_secondary_startup_failure();
}

fn idle_thread(_arg: sched::ThreadArg) -> usize {
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
