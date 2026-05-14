//! C ABI surface для arch-agnostic `kernel/`.
//!
//! `kernel/` не зависит от Rust API `arch/aarch64`, поэтому все PA/HVA и VM
//! операции пересекают границу через `extern "C"` функции. Этот модуль только
//! адаптирует ABI к HVA/page-table implementation.

use core::ptr::write_volatile;

use super::hva::{
    VmError, VmFlags, drop_boot_identity_mapping, hva_to_phys, map_kernel_region, phys_to_hva,
    protect_kernel_region, switch_to_runtime_kernel_tables, translate_kernel_va,
    unmap_kernel_region, vm_attr_from_code, vm_error_code,
};

#[unsafe(no_mangle)]
pub extern "C" fn arch_phys_to_virt(pa: usize) -> usize {
    phys_to_hva(pa)
}

#[unsafe(no_mangle)]
pub extern "C" fn arch_virt_to_phys(va: usize) -> usize {
    hva_to_phys(va)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn arch_drop_boot_identity_mapping() -> u64 {
    match unsafe { drop_boot_identity_mapping() } {
        Ok(()) => 0,
        Err(err) => vm_error_code(err),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn arch_switch_to_runtime_kernel_tables() -> u64 {
    match unsafe { switch_to_runtime_kernel_tables() } {
        Ok(()) => 0,
        Err(err) => vm_error_code(err),
    }
}

/// C ABI wrapper: перевести kernel VA через текущие TTBR1 page tables.
///
/// Возвращает `0` при успехе и пишет PA в `out_pa`; остальные значения
/// соответствуют `vm_error_code()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arch_translate_kernel_va(va: usize, out_pa: *mut usize) -> u64 {
    if out_pa.is_null() {
        return vm_error_code(VmError::InvalidRange);
    }

    match translate_kernel_va(va) {
        Some(pa) => {
            unsafe { write_volatile(out_pa, pa) };
            0
        }
        None => vm_error_code(VmError::MissingMapping),
    }
}

/// C ABI wrapper для post-init map. Только TTBR1 kernel VA, 2 MiB-aligned
/// regions; вызов не предназначен для IRQ fast path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arch_map_kernel_region(
    va: usize,
    pa: usize,
    size: usize,
    attr: u32,
    flags: u64,
) -> u64 {
    let attr = match vm_attr_from_code(attr) {
        Ok(attr) => attr,
        Err(err) => return vm_error_code(err),
    };

    match unsafe { map_kernel_region(va, pa, size, attr, VmFlags::from_bits(flags)) } {
        Ok(()) => 0,
        Err(err) => vm_error_code(err),
    }
}

/// C ABI wrapper для удаления TTBR1 mappings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arch_unmap_kernel_region(va: usize, size: usize) -> u64 {
    match unsafe { unmap_kernel_region(va, size) } {
        Ok(()) => 0,
        Err(err) => vm_error_code(err),
    }
}

/// C ABI wrapper для изменения access flags существующего 2 MiB block mapping.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arch_protect_kernel_region(va: usize, size: usize, flags: u64) -> u64 {
    match unsafe { protect_kernel_region(va, size, VmFlags::from_bits(flags)) } {
        Ok(()) => 0,
        Err(err) => vm_error_code(err),
    }
}
