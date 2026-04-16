#[inline(always)]
pub fn ec(raw_esr: u64) -> u8 {
    ((raw_esr >> 26) & 0x3f) as u8
}

#[inline(always)]
pub fn iss(raw_esr: u64) -> u32 {
    (raw_esr & 0x01ff_ffff) as u32
}

pub fn ec_name(ec: u8) -> &'static str {
    match ec {
        0b000000 => "Unknown reason",
        0b001110 => "Illegal execution state",
        0b010001 => "SVC instruction (AArch32)",
        0b010101 => "SVC instruction (AArch64)",
        0b011000 => "MSR/MRS/System instruction trap (AArch64)",
        0b100000 => "Instruction abort, lower EL",
        0b100001 => "Instruction abort, same EL",
        0b100010 => "PC alignment fault",
        0b100100 => "Data abort, lower EL",
        0b100101 => "Data abort, same EL",
        0b100110 => "SP alignment fault",
        0b110000 => "Breakpoint, lower EL",
        0b110001 => "Breakpoint, same EL",
        0b111100 => "BRK instruction (AArch64)",
        _ => "Unclassified/unsupported EC",
    }
}
