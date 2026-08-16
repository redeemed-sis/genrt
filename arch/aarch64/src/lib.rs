#![no_std]

use core::{
    arch::{asm, global_asm},
    sync::atomic::{AtomicU64, Ordering},
};

use bootinfo::BootInfo;

mod console;
mod context;
mod esr;
mod exception;
mod gic;
mod mmio;
mod mmu;
mod platform;
mod timer;
mod trap_frame;

global_asm!(include_str!("boot.s"));
global_asm!(include_str!("exceptions.s"));

#[unsafe(no_mangle)]
pub static BOOT_CURRENT_EL: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    static __bss_start: u8;
    static __bss_end: u8;
    static __vectors: u8;
    static __secondary_start_value: usize;
    static __secondary_ttbr1_value: usize;
    static __boot_stack_bottom_value: usize;
    static __boot_stack_top_value: usize;
}

#[unsafe(no_mangle)]
/// Enter high-linked Rust on the boot CPU after low MMU bring-up.
///
/// This is the sole global architecture initialization path. It clears the
/// high BSS, publishes platform and CPU topology, initializes CPU0-local
/// interrupt state, and enters the generic kernel.
///
/// # Arguments
///
/// * `boot_mmu_params_pa` - Physical address of low immutable boot MMU and
///   platform parameters prepared by `_start`.
/// * `bootstrap_slot` - Architecture stack slot selected by `_start`; it must
///   be zero for CPU0.
///
/// # Returns
///
/// This function never returns.
///
/// # Panics
///
/// Panics through the fatal path when stack ownership, platform parsing, CPU
/// topology, or generic boot initialization is invalid.
///
/// # Safety
///
/// `_start` must call this exactly once on CPU0 after enabling the boot
/// translation regime, with a valid low parameter address and high stack.
pub extern "C" fn rust_entry(boot_mmu_params_pa: usize, bootstrap_slot: usize) -> ! {
    if bootstrap_slot != 0 {
        arch_hard_fault();
    }
    unsafe {
        zero_bss();
        mmu::init_from_boot_params(boot_mmu_params_pa);
        install_vectors();
        BOOT_CURRENT_EL.store(current_el(), Ordering::Relaxed);
    }
    let boot_platform_params = mmu::platform_params_from_boot_params(boot_mmu_params_pa);
    let (dtb_pa, dtb_size) = platform::dtb_from_boot_params(boot_platform_params);
    let dtb_va = mmu::phys_to_hva(dtb_pa);
    let platform_info = platform::info_from_boot_params(boot_platform_params);
    let platform = unsafe {
        platform::init(platform_info)
            .unwrap_or_else(|err| panic!("arch: invalid AArch64 platform info: {err:?}"))
    };
    console::configure_from_platform(platform);
    gic::configure_from_platform(platform);
    let dtb = unsafe { core::slice::from_raw_parts(dtb_va as *const u8, dtb_size as usize) };
    platform::init_cpu_topology(dtb, arch_current_cpu_hardware_id())
        .unwrap_or_else(|err| panic!("arch: invalid AArch64 CPU topology: {err:?}"));
    let bootinfo: &'static BootInfo = unsafe { kernel::boot::init_bootinfo(dtb_pa, dtb_va) };
    if bootinfo.cpu_count as usize != platform::cpu_count() {
        panic!("arch: generic and AArch64 CPU topology counts differ");
    }
    unsafe {
        gic::init_controller_minimal();
        gic::enable_irq(timer::TIMER_IRQ_ID_PHYS, 0x40);
        console::enable_rx_interrupts();
        gic::enable_irq(platform::qemu::UART0_IRQ_ID, 0x60);
        timer::early_init();
    }
    kernel::kernel_main(bootinfo)
}

/// Enter generic parked-secondary startup after architecture MMU bring-up.
///
/// # Arguments
///
/// * `_boot_mmu_params_pa` - Physical address of the immutable boot MMU
///   parameters already installed by the assembly trampoline.
/// * `bootstrap_slot` - Unique architecture-assigned bootstrap stack slot.
///
/// # Returns
///
/// This function never returns. It performs only CPU-local vector/interrupt
/// setup before delegating registration and parked-state ownership to the
/// generic kernel.
///
/// # Safety
///
/// `secondary_start` must call this after installing the published runtime
/// TTBR1, selecting the matching private high stack, and preserving masked
/// asynchronous exceptions.
#[unsafe(no_mangle)]
pub extern "C" fn secondary_rust_entry(_boot_mmu_params_pa: usize, bootstrap_slot: usize) -> ! {
    // SAFETY: the secondary owns its DAIF and VBAR_EL1 registers. All
    // asynchronous exceptions remain masked throughout this milestone.
    unsafe {
        asm!(
            "msr daifset, #0xf",
            "isb",
            options(nomem, nostack, preserves_flags)
        );
        install_vectors();
        clear_local_ttbr0();
    }
    kernel::kernel_secondary_main(bootstrap_slot)
}

#[unsafe(no_mangle)]
/// Return the CPU count in the immutable AArch64 platform topology.
///
/// # Returns
///
/// Returns the bounded enabled CPU count parsed from the runtime DTB. The
/// query allocates nothing and does not alter local IRQ state.
pub extern "C" fn arch_expected_cpu_count() -> usize {
    platform::cpu_count()
}

#[unsafe(no_mangle)]
/// Return the number of linker-owned AArch64 bootstrap stack slots.
///
/// # Returns
///
/// Returns the fixed stack-array extent divided by the 32 KiB slot size. The
/// calculation allocates nothing and does not access stack contents.
pub extern "C" fn arch_bootstrap_stack_capacity() -> usize {
    let bottom = unsafe { core::ptr::addr_of!(__boot_stack_bottom_value).read_volatile() };
    let top = unsafe { core::ptr::addr_of!(__boot_stack_top_value).read_volatile() };
    top.saturating_sub(bottom) / (32 * 1024)
}

#[unsafe(no_mangle)]
/// Verify the executing CPU against one platform-selected logical slot.
///
/// # Arguments
///
/// * `logical_index` - CPU0-selected logical slot passed through PSCI context.
///
/// # Returns
///
/// Returns `true` when the normalized executing MPIDR identity equals the
/// immutable DTB hardware identity for that slot.
pub extern "C" fn arch_secondary_cpu_identity_matches(logical_index: usize) -> bool {
    platform::expected_hardware_id(logical_index) == Some(arch_current_cpu_hardware_id())
}

#[unsafe(no_mangle)]
/// Start one platform-described secondary CPU through PSCI HVC.
///
/// # Arguments
///
/// * `logical_index` - Checked nonzero logical/stack slot selected by CPU0.
///
/// # Returns
///
/// Returns the signed PSCI status. Zero means CPU_ON accepted the request;
/// every other value is a controlled startup failure. The call publishes the
/// live runtime TTBR1 root before entering the hypervisor and allocates nothing.
///
/// # Safety
///
/// CPU0 must invoke this only after runtime TTBR1 and immutable CPU topology
/// publication. The selected slot must not already have been started.
pub extern "C" fn arch_start_secondary_cpu(logical_index: usize) -> i64 {
    let Some((function_id, target_hardware)) = platform::psci_cpu_on_call(logical_index) else {
        return -2;
    };
    let entry_pa = unsafe { core::ptr::addr_of!(__secondary_start_value).read_volatile() };
    let ttbr1 = read_ttbr1();
    let handoff_pa = unsafe { core::ptr::addr_of!(__secondary_ttbr1_value).read_volatile() };
    let handoff = mmu::phys_to_hva(handoff_pa) as *const AtomicU64;
    // SAFETY: CPU0 is the sole starter and publishes one immutable live TTBR1
    // value before any CPU_ON request. The low secondary entry observes it with
    // an Acquire load.
    unsafe { (&*handoff).store(ttbr1, Ordering::Release) };
    // SAFETY: the barrier orders the handoff before the PSCI call. QEMU's DTB
    // selected the HVC conduit and supplied the CPU_ON function ID/target.
    unsafe {
        asm!("dsb ishst", options(nostack, preserves_flags));
    }
    let mut status = function_id;
    let target = target_hardware;
    let entry = entry_pa;
    let context = logical_index;
    // SAFETY: this follows the PSCI CPU_ON calling convention. The entry is a
    // low physical `.boot.text` symbol and x3 is returned to it as context_id.
    unsafe {
        asm!(
            "hvc #0",
            inout("x0") status,
            inout("x1") target => _,
            inout("x2") entry => _,
            inout("x3") context => _,
            options(nostack)
        );
    }
    status as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_irq_enable() {
    // SAFETY: Called once the kernel is ready to receive timer IRQs.
    unsafe { timer::enable_cpu_irq() }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_local_irq_save_and_disable() -> u64 {
    let saved_daif: u64;
    unsafe {
        asm!(
            "mrs {saved_daif}, DAIF",
            "msr daifset, #2",
            "isb",
            saved_daif = out(reg) saved_daif,
            options(nomem, nostack, preserves_flags)
        );
    }
    saved_daif
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_local_irq_restore(saved_daif: u64) {
    unsafe {
        asm!(
            "msr DAIF, {saved_daif}",
            "isb",
            saved_daif = in(reg) saved_daif,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[unsafe(no_mangle)]
/// Classify whether restoring an opaque saved IRQ state permits an EL1 sched call.
///
/// # Arguments
///
/// * `saved_daif` - DAIF value returned by the local-IRQ save hook.
///
/// # Returns
///
/// Returns `true` when IRQ delivery would be enabled after restoration. This
/// is a register-only, allocation-free architecture classification.
pub extern "C" fn arch_irq_state_allows_sched_call(saved_daif: u64) -> bool {
    // DAIF.I is bit 7. The generic kernel asks only whether restoring the saved
    // state permits a controlled EL1 sched call; it never interprets DAIF.
    (saved_daif & (1 << 7)) == 0
}

#[unsafe(no_mangle)]
/// Return the normalized hardware identity of the executing AArch64 CPU.
///
/// # Returns
///
/// Returns MPIDR affinity levels Aff3:Aff0 packed without the architecture
/// control bits.  The read is register-only, allocation-free, non-blocking,
/// and leaves IRQ state unchanged.  Generic kernel code treats this as opaque.
pub extern "C" fn arch_current_cpu_hardware_id() -> u64 {
    let mpidr: u64;
    // SAFETY: MPIDR_EL1 is a read-only architected system register at EL1.
    unsafe {
        asm!(
            "mrs {mpidr}, MPIDR_EL1",
            mpidr = out(reg) mpidr,
            options(nomem, nostack, preserves_flags)
        );
    }
    // Aff0 is bits 7:0; Aff1..3 are bits 15:8, 23:16, and 39:32.  The
    // resulting compact value is a hardware key, never a scheduler index.
    normalize_mpidr(mpidr)
}

#[unsafe(no_mangle)]
/// Bind a registered logical CPU index to the executing AArch64 CPU.
///
/// # Arguments
///
/// * `logical_index` - Checked zero-based logical CPU storage index assigned by
///   the generic boot registry.
///
/// # Returns
///
/// Returns nothing. The index is stored in `TPIDR_EL1` as `logical_index + 1`,
/// reserving zero for an unbound CPU. The operation is register-only,
/// allocation-free, non-blocking, and leaves IRQ state unchanged.
pub extern "C" fn arch_bind_current_cpu_logical_id(logical_index: usize) {
    let encoded = logical_index
        .checked_add(1)
        .expect("cpu: logical ID encoding overflow");
    // SAFETY: TPIDR_EL1 is software-owned at EL1. Boot clears it on every CPU,
    // and the generic registry calls this hook only with a checked logical ID.
    unsafe {
        asm!(
            "msr TPIDR_EL1, {encoded}",
            encoded = in(reg) encoded,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[unsafe(no_mangle)]
/// Return the encoded logical identity bound to the executing AArch64 CPU.
///
/// # Returns
///
/// Returns zero when the CPU is unbound, or the registered logical index plus
/// one after binding. The operation is register-only, allocation-free,
/// non-blocking, and leaves IRQ state unchanged.
pub extern "C" fn arch_current_cpu_logical_id() -> usize {
    let encoded: usize;
    // SAFETY: TPIDR_EL1 is software-owned and initialized by the boot
    // trampoline before generic kernel code can read it.
    unsafe {
        asm!(
            "mrs {encoded}, TPIDR_EL1",
            encoded = out(reg) encoded,
            options(nomem, nostack, preserves_flags)
        );
    }
    encoded
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_counter_now() -> u64 {
    timer::counter()
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_counter_freq_hz() -> u64 {
    timer::frequency_hz()
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_timer_arm_deadline(deadline: u64) {
    // SAFETY: kernel passes an absolute architected-counter deadline.
    unsafe { timer::arm_deadline(deadline) }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_timer_disarm() {
    // SAFETY: kernel explicitly disables the timer when no deadlines are pending.
    unsafe { timer::disable() }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_sched_call(request: *const core::ffi::c_void) {
    // SAFETY: `svc #0` raises a synchronous exception at the current EL. The
    // EL1 vector path saves the current TrapFrame and routes the request pointer
    // through `sync_entry()`. If the request blocks, execution resumes after this
    // instruction when the thread is later woken.
    unsafe {
        asm!(
            "svc #0",
            in("x0") request,
            options(nostack)
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_initramfs_load_pa() -> usize {
    platform::qemu::INITRAMFS_LOAD_PA
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_initramfs_reserved_size() -> usize {
    platform::qemu::INITRAMFS_RESERVED_SIZE
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_hard_fault() -> ! {
    // SAFETY: this path is terminal by contract; IRQ/FIQ/SError are masked first.
    unsafe {
        asm!(
            "msr daifset, #0xf",
            options(nomem, nostack, preserves_flags)
        );
        asm!("isb", options(nomem, nostack, preserves_flags));
    }

    loop {
        // SAFETY: WFE loop is a deterministic hard-stop in early bring-up.
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Park the executing secondary CPU with asynchronous exceptions masked.
///
/// # Returns
///
/// This function never returns. It executes an allocation-free architecture
/// wait loop and does not initialize interrupts, timers, or scheduling.
#[unsafe(no_mangle)]
pub extern "C" fn arch_park_current_cpu() -> ! {
    // SAFETY: parked secondary CPUs own their local DAIF state and have no
    // runnable context to preserve.
    unsafe {
        asm!(
            "msr daifset, #0xf",
            "isb",
            options(nomem, nostack, preserves_flags)
        );
    }
    loop {
        // SAFETY: WFE is the controlled architecture idle state for a parked
        // CPU. Events may wake it transiently, after which it immediately waits
        // again without observing runtime state.
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

unsafe fn clear_local_ttbr0() {
    // SAFETY: the secondary has already branched to TTBR1 high-half code. This
    // clears only its local temporary identity root and invalidates local EL1
    // translations; shared page-table memory is left untouched.
    unsafe {
        asm!(
            "msr TTBR0_EL1, xzr",
            "isb",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags)
        );
    }
}

fn read_ttbr1() -> u64 {
    let value: u64;
    // SAFETY: TTBR1_EL1 is CPU-local architected state readable at EL1.
    unsafe {
        asm!(
            "mrs {value}, TTBR1_EL1",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

const fn normalize_mpidr(mpidr: u64) -> u64 {
    (mpidr & 0x00ff_ffff) | ((mpidr >> 8) & 0xff00_0000)
}

unsafe fn zero_bss() {
    let start = core::ptr::addr_of!(__bss_start) as usize;
    let end = core::ptr::addr_of!(__bss_end) as usize;
    let len = end.saturating_sub(start);
    // SAFETY: `rust_entry` runs once after the MMU high mapping is live and
    // before Rust globals are observed by normal kernel code.
    unsafe { core::ptr::write_bytes(start as *mut u8, 0, len) };
}

unsafe fn install_vectors() {
    let vectors = core::ptr::addr_of!(__vectors) as usize;
    // SAFETY: `__vectors` is high-linked and mapped through TTBR1 before entry.
    unsafe {
        asm!(
            "msr VBAR_EL1, {vectors}",
            "isb",
            vectors = in(reg) vectors,
            options(nostack, preserves_flags)
        );
    }
}

fn current_el() -> u64 {
    let value: u64;
    unsafe {
        asm!(
            "mrs {value}, CurrentEL",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}
