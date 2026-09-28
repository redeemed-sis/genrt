//! Raspberry Pi executable-page cache publication.

use crate::mmu;

/// Make a populated executable user page visible to instruction fetch.
///
/// # Arguments
///
/// * `pa` - Physical address of the unpublished 4 KiB page.
///
/// # Returns
///
/// Returns nothing after cleaning data to PoU and invalidating the shared
/// instruction caches. Runs in task context without allocation or blocking.
/// The caller owns the populated page until its mapping is published.
///
/// ELF/fork filled this page through the writable direct map; Cortex-A53
/// instruction fetch is not automatically coherent with those stores.
///
/// # Safety
///
/// `pa` must identify the caller-owned, populated 4 KiB physical page. Its
/// direct-map alias must be accessible until publication completes.
pub(crate) unsafe fn synchronize_loaded_code(pa: usize) {
    use core::arch::asm;
    let ctr: usize;
    // SAFETY: the caller owns an allocated, populated 4 KiB code page, not yet
    // runnable. Cache maintenance uses its mapped direct alias in task context.
    unsafe {
        asm!("mrs {ctr}, CTR_EL0", ctr = out(reg) ctr, options(nomem, nostack));
        let line = 4 << ((ctr >> 16) & 15);
        let start = mmu::phys_to_hva(pa);
        let mut address = start;
        while address < start + 4096 {
            asm!("dc cvau, {address}", address = in(reg) address, options(nostack));
            address += line;
        }
        asm!("dsb ish", "ic ialluis", "dsb ish", "isb", options(nostack));
    }
}
