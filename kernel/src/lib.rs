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
/// starts architecture-initialized parked secondaries, and then enters the sole
/// online scheduler context. CPU0-local interrupt setup is complete before the
/// architecture calls this entry.
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
    cpu::start_and_park_secondaries()
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

/// Prepare generic state for one architecture-started secondary CPU.
///
/// This is the first generic stage of secondary startup. It registers the
/// executing CPU's hardware identity and logical bootstrap slot, then verifies
/// that no scheduler context is online for the secondary. Architecture code
/// retains ownership of exception-vector, interrupt-controller, timer, and
/// parking setup before and after this call.
///
/// # Arguments
///
/// * `logical_index` - CPU0-selected logical slot carried through PSCI and used
///   by the architecture for the matching private bootstrap stack.
///
/// # Returns
///
/// Returns `true` when generic registration succeeds. Returns `false` after
/// publishing a secondary-startup failure when the logical identity is stale,
/// duplicated, out of order, or already owns an online scheduler context.
///
/// The function is bounded and allocation-free. It does not enable IRQ,
/// initialize architecture state, or enter the scheduler.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_prepare_secondary_cpu(logical_index: usize) -> bool {
    let result = cpu::register_secondary_cpu(logical_index).and_then(|id| {
        if sched::scheduler_online(id) {
            Err(cpu::CpuRegistrationError::UnexpectedCpu)
        } else {
            Ok(())
        }
    });
    if result.is_err() {
        cpu::record_secondary_startup_failure();
    }
    result.is_ok()
}

/// Publish completion of architecture-owned secondary CPU initialization.
///
/// Architecture code calls this only after local exception vectors, GIC CPU
/// interface, timer PPI, and physical timer state are ready. The function
/// verifies the registered logical identity, optionally arms the SMP contract
/// timer probe, and publishes the terminal parked boundary observed by CPU0.
/// No architecture-ready flag is stored: successful completion is the single
/// generic publication that architecture-local setup has finished.
///
/// # Arguments
///
/// * `logical_index` - Logical bootstrap slot previously accepted by
///   [`kernel_prepare_secondary_cpu`].
///
/// # Returns
///
/// Returns `true` when completion is published. Returns `false` after
/// recording a startup failure when the executing CPU does not match
/// `logical_index`, unexpectedly owns an online scheduler context, or cannot
/// publish its parked state.
///
/// The function is bounded and allocation-free. IRQ must remain masked while
/// it runs; architecture code chooses the subsequent parked state.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_complete_secondary_cpu_startup(logical_index: usize) -> bool {
    let result = cpu::current_id().and_then(|id| {
        if id.index() != logical_index || sched::scheduler_online(id) {
            return Err(cpu::CpuRegistrationError::UnexpectedCpu);
        }
        #[cfg(feature = "qemu-test-smp-boot")]
        time::arm_local_timer_probe_for_test();
        cpu::mark_secondary_parked(id)
    });
    if result.is_err() {
        cpu::record_secondary_startup_failure();
    }
    result.is_ok()
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
