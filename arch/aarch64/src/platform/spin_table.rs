//! Pi armstub8 release words: little-endian entry PA, clean to PoC, then SEV.
//! The low firmware reservation is never handed to the frame allocator.

use core::arch::asm;

/// Release secondary `index` at `entry_pa` after publishing its translation root.
///
/// # Arguments
/// * `index` - Validated logical CPU / bootstrap slot, in 1..4.
/// * `entry_pa` - Shared low-linked secondary entry, prepared by the loader.
/// * `handoff_pa` - Reserved boot word already containing the runtime root.
/// * `ttbr1` - Live allocator-owned kernel translation root physical address.
///
/// # Returns
/// Zero after release publication, or -2 for an absent/invalid CPU. Boot-only,
/// allocation-free. CPU0 must be the sole releaser; tables must remain stable
/// during traversal. Scheduler-online acknowledgement belongs to generic boot.
pub(crate) fn start(index: usize, entry_pa: usize, handoff_pa: usize, ttbr1: usize) -> i64 {
    let Some(release_pa) = super::cpu::spin_release_address(index) else {
        return -2;
    };
    let line = cache_line_size();
    // A secondary reads handoff state with translation disabled. Clean tables
    // as well, so startup does not depend on cacheable page-walk speculation.
    clean_tables(ttbr1, 0, line);
    clean_range(crate::mmu::phys_to_hva(handoff_pa), 8, line);
    let release = crate::mmu::phys_to_hva(release_pa);
    // SAFETY: DTB validation admits only reserved armstub8 release words. CPU0
    // owns one publication per word; firmware owns only the observing load.
    unsafe { (release as *mut u64).write_volatile((entry_pa as u64).to_le()) };
    clean_range(release, 8, line);
    // SAFETY: complete PoC visibility before waking the firmware WFE loop.
    unsafe { asm!("dsb sy", "sev", options(nostack, preserves_flags)) };
    0
}

fn cache_line_size() -> usize {
    let ctr: usize;
    // SAFETY: read-only architectural cache geometry register.
    unsafe { asm!("mrs {ctr}, CTR_EL0", ctr = out(reg) ctr, options(nomem, nostack)) };
    4 << ((ctr >> 16) & 15)
}

fn clean_range(start: usize, size: usize, line: usize) {
    let mut address = start & !(line - 1);
    while address < start + size {
        // SAFETY: caller passes a mapped normal-memory interval owned by boot.
        unsafe { asm!("dc cvac, {address}", address = in(reg) address, options(nostack)) };
        address += line;
    }
}

fn clean_tables(pa: usize, level: usize, line: usize) {
    const TABLE_ADDRESS: u64 = 0x0000_ffff_ffff_f000;
    let va = crate::mmu::phys_to_hva(pa);
    if level < 3 {
        for index in 0..512 {
            // SAFETY: the live root reaches only allocated 4 KiB table pages;
            // boot excludes mapping mutation and the depth is bounded to four.
            let descriptor = unsafe { (va as *const u64).add(index).read_volatile() };
            if descriptor & 3 == 3 {
                clean_tables((descriptor & TABLE_ADDRESS) as usize, level + 1, line);
            }
        }
    }
    clean_range(va, 4096, line);
}
