use core::arch::asm;

use crate::{esr, gic, timer, trap_frame::TrapFrame};

#[unsafe(no_mangle)]
pub extern "C" fn irq_entry(frame: *mut TrapFrame) {
    // SAFETY: exception entry assembly always passes a valid trap frame pointer.
    let frame_words = frame as *mut u64;

    let iar = gic::acknowledge_irq();
    let irq_id = gic::irq_id_from_iar(iar);

    if !gic::is_spurious(irq_id) {
        if irq_id == timer::TIMER_IRQ_ID_PHYS {
            timer::on_timer_irq(frame_words);
        } else {
            puts("[irq] unexpected id=0x");
            put_hex_u32(irq_id);
            puts("\n");
        }
        gic::end_irq(iar);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn exception_entry(vector: u64, frame: *const TrapFrame) -> ! {
    // SAFETY: fatal exception handling is terminal; masking interrupts makes
    // the diagnostic path deterministic even if a future caller forgets to.
    unsafe {
        asm!(
            "msr daifset, #0xf",
            options(nomem, nostack, preserves_flags)
        );
        asm!("isb", options(nomem, nostack, preserves_flags));
    }

    let (source, kind) = vector_source_kind(vector);
    let raw_esr = read_esr_el1();
    let raw_far = read_far_el1();
    let raw_current_el = read_current_el();
    let raw_spsr = read_spsr_el1();
    let ec = esr::ec(raw_esr);
    let iss = esr::iss(raw_esr);

    puts("\n[exception] fatal\n");
    puts("[exception] source=");
    puts(source);
    puts(" kind=");
    puts(kind);
    puts("\n");

    puts("[exception] CurrentEL=0x");
    put_hex_u64(raw_current_el);
    puts(" EL=");
    put_hex_u64((raw_current_el >> 2) & 0x3);
    puts("\n");

    puts("[exception] ESR_EL1=0x");
    put_hex_u64(raw_esr);
    puts(" EC=0x");
    put_hex_u64(ec as u64);
    puts(" (");
    puts(esr::ec_name(ec));
    puts(") ISS=0x");
    put_hex_u32(iss);
    puts("\n");

    puts("[exception] FAR_EL1=0x");
    put_hex_u64(raw_far);
    puts("\n");

    puts("[exception] ELR_EL1=0x");
    put_hex_u64(read_elr_el1());
    puts("\n");

    puts("[exception] SPSR_EL1=0x");
    put_hex_u64(raw_spsr);
    puts("\n");

    if frame.is_null() {
        puts("[exception] trap_frame=<null>\n");
    } else {
        // SAFETY: exception entry assembly passes a live saved trap frame.
        let tf = unsafe { &*frame };
        puts("[exception] tf.x0=0x");
        put_hex_u64(tf.x[0]);
        puts(" tf.x1=0x");
        put_hex_u64(tf.x[1]);
        puts(" tf.x2=0x");
        put_hex_u64(tf.x[2]);
        puts(" tf.x3=0x");
        put_hex_u64(tf.x[3]);
        puts("\n");

        puts("[exception] tf.x29=0x");
        put_hex_u64(tf.x[29]);
        puts(" tf.x30=0x");
        put_hex_u64(tf.x[30]);
        puts("\n");

        puts("[exception] tf.sp=0x");
        put_hex_u64(tf.sp);
        puts(" tf.elr=0x");
        put_hex_u64(tf.elr);
        puts(" tf.spsr=0x");
        put_hex_u64(tf.spsr);
        puts("\n");
    }

    crate::arch_hard_fault()
}

#[inline(always)]
fn vector_source_kind(vector: u64) -> (&'static str, &'static str) {
    match vector {
        0 => ("current_el_sp0", "sync"),
        1 => ("current_el_sp0", "irq"),
        2 => ("current_el_sp0", "fiq"),
        3 => ("current_el_sp0", "serror"),
        4 => ("current_el_spx", "sync"),
        5 => ("current_el_spx", "irq"),
        6 => ("current_el_spx", "fiq"),
        7 => ("current_el_spx", "serror"),
        8 => ("lower_el_aarch64", "sync"),
        9 => ("lower_el_aarch64", "irq"),
        10 => ("lower_el_aarch64", "fiq"),
        11 => ("lower_el_aarch64", "serror"),
        12 => ("lower_el_aarch32", "sync"),
        13 => ("lower_el_aarch32", "irq"),
        14 => ("lower_el_aarch32", "fiq"),
        15 => ("lower_el_aarch32", "serror"),
        _ => ("unknown", "unknown"),
    }
}

#[inline(always)]
fn read_esr_el1() -> u64 {
    let value: u64;
    // SAFETY: reading system register for exception diagnostics in EL1.
    unsafe {
        asm!(
            "mrs {value}, ESR_EL1",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
fn read_far_el1() -> u64 {
    let value: u64;
    // SAFETY: reading system register for exception diagnostics in EL1.
    unsafe {
        asm!(
            "mrs {value}, FAR_EL1",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
fn read_elr_el1() -> u64 {
    let value: u64;
    // SAFETY: reading system register for exception diagnostics in EL1.
    unsafe {
        asm!(
            "mrs {value}, ELR_EL1",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
fn read_spsr_el1() -> u64 {
    let value: u64;
    // SAFETY: reading system register for exception diagnostics in EL1.
    unsafe {
        asm!(
            "mrs {value}, SPSR_EL1",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
fn read_current_el() -> u64 {
    let value: u64;
    // SAFETY: reading current exception level for diagnostics.
    unsafe {
        asm!(
            "mrs {value}, CurrentEL",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline(always)]
fn puts(s: &str) {
    for b in s.bytes() {
        putc(b);
    }
}

#[inline(always)]
fn putc(c: u8) {
    crate::console::arch_console_init_once();
    if c == b'\n' {
        crate::console::arch_console_putc_raw(b'\r');
    }
    crate::console::arch_console_putc_raw(c);
}

#[inline(always)]
fn put_hex_u32(value: u32) {
    put_hex(value as u64, 8);
}

#[inline(always)]
fn put_hex_u64(value: u64) {
    put_hex(value, 16);
}

#[inline(always)]
fn put_hex(value: u64, digits: usize) {
    for shift in (0..digits).rev() {
        let nibble = ((value >> (shift * 4)) & 0xF) as u8;
        putc(if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        });
    }
}
