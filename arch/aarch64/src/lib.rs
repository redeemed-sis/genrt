#![no_std]

use core::arch::{asm, global_asm};

use bootinfo::BootInfo;

mod console;
mod esr;
mod exception;
mod gic;
mod mmio;
mod mmu;
mod platform;
mod timer;
mod trap_frame;

use trap_frame::TrapFrame;

global_asm!(include_str!("boot.s"));
global_asm!(include_str!("exceptions.s"));

#[unsafe(no_mangle)]
pub static mut BOOT_CURRENT_EL: u64 = 0;

unsafe extern "C" {
    static __bss_start: u8;
    static __bss_end: u8;
    static __vectors: u8;
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_entry(boot_mmu_params_pa: usize) -> ! {
    unsafe {
        zero_bss();
        mmu::init_from_boot_params(boot_mmu_params_pa);
        install_vectors();
        BOOT_CURRENT_EL = current_el();
    }
    let boot_platform_params = mmu::platform_params_from_boot_params(boot_mmu_params_pa);
    let (dtb_pa, _) = platform::dtb_from_boot_params(boot_platform_params);
    let dtb_va = mmu::phys_to_hva(dtb_pa);
    let platform_info = platform::info_from_boot_params(boot_platform_params);
    let platform = unsafe {
        platform::init(platform_info)
            .unwrap_or_else(|err| panic!("arch: invalid AArch64 platform info: {err:?}"))
    };
    console::configure_from_platform(platform);
    gic::configure_from_platform(platform);
    let bootinfo: &'static BootInfo = unsafe { kernel::boot::init_bootinfo(dtb_pa, dtb_va) };
    unsafe {
        gic::init_controller_minimal();
        gic::enable_irq(timer::TIMER_IRQ_ID_PHYS, 0x40);
        console::enable_rx_interrupts();
        gic::enable_irq(platform::qemu::UART0_IRQ_ID, 0x60);
        timer::early_init();
    }
    kernel::kernel_main(bootinfo)
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
pub extern "C" fn arch_task_call(request: *const core::ffi::c_void) {
    // SAFETY: `svc #0` raises a synchronous exception at the current EL. The
    // EL1 vector path saves the current TrapFrame and routes the request pointer
    // through `sync_entry()`. If the request blocks, execution resumes after this
    // instruction when the task is later woken.
    unsafe {
        asm!(
            "svc #0",
            in("x0") request,
            options(nostack)
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_restart_current_syscall(frame_words: *mut u64) {
    if frame_words.is_null() {
        return;
    }

    // SAFETY: lower-EL syscall dispatch passes a live TrapFrame pointer. AArch64
    // `svc #imm` is a fixed 4-byte instruction, so subtracting 4 from ELR_EL1
    // restarts the same userspace syscall after the blocked thread wakes.
    let frame = unsafe { &mut *(frame_words as *mut TrapFrame) };
    frame.elr = frame
        .elr
        .checked_sub(4)
        .unwrap_or_else(|| panic!("arch: cannot restart syscall at ELR=0"));
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_init_thread_frame(
    frame_words: *mut u64,
    stack_top: usize,
    entry_addr: usize,
    arg: usize,
    bootstrap_pc: usize,
) {
    if frame_words.is_null() {
        return;
    }

    // SAFETY: kernel passes valid task-owned frame storage matching TrapFrame ABI.
    let frame = unsafe { &mut *(frame_words as *mut TrapFrame) };
    frame.init_kernel_el1(bootstrap_pc, stack_top, entry_addr, arg);
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_init_user_trap_frame(
    frame_words: *mut u64,
    user_entry: usize,
    user_sp: usize,
    kernel_sp: usize,
    arg0: usize,
) {
    if frame_words.is_null() {
        return;
    }

    // SAFETY: caller passes task-owned frame storage matching TrapFrame ABI.
    let frame = unsafe { &mut *(frame_words as *mut TrapFrame) };
    frame.init_user_el0(user_entry, user_sp, kernel_sp, arg0);
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_clone_user_trap_frame_for_fork(
    dst_frame_words: *mut u64,
    src_frame_words: *const u64,
    child_kernel_sp: usize,
) {
    if dst_frame_words.is_null() || src_frame_words.is_null() {
        return;
    }

    // SAFETY: scheduler passes valid TrapFrame storage for both pointers. The
    // child inherits the userspace resume state, but fork returns 0 in the child
    // and must use a distinct EL1 kernel stack for future lower-EL exceptions.
    let src = unsafe { &*(src_frame_words as *const TrapFrame) };
    let dst = unsafe { &mut *(dst_frame_words as *mut TrapFrame) };
    *dst = *src;
    dst.x[0] = 0;
    dst.kernel_sp = child_kernel_sp as u64 & !0xf;
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_init_user_exec_frame(
    frame_words: *mut u64,
    user_entry: usize,
    user_sp: usize,
) {
    if frame_words.is_null() {
        return;
    }

    // SAFETY: lower-EL syscall dispatch passes the live TrapFrame for the
    // current user thread. execve preserves the thread's EL1 kernel stack while
    // replacing all EL0-visible register state and resume PC/SP.
    let frame = unsafe { &mut *(frame_words as *mut TrapFrame) };
    let kernel_sp = frame.kernel_sp as usize;
    frame.init_user_el0(user_entry, user_sp, kernel_sp, 0);
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
