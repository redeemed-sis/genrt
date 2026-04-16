#[repr(C)]
#[derive(Copy, Clone)]
pub struct TrapFrame {
    pub x: [u64; 31],
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
}

impl TrapFrame {
    // Assembly contract (`exceptions.s`) depends on these offsets and size.
    pub const OFFSET_X0: usize = 0;
    pub const OFFSET_X30: usize = 30 * 8;
    pub const OFFSET_SP: usize = 31 * 8;
    pub const OFFSET_ELR: usize = 32 * 8;
    pub const OFFSET_SPSR: usize = 33 * 8;
    pub const SIZE_BYTES: usize = 34 * 8;
    pub const WORDS: usize = 34;
    pub const EL1H: u64 = 0b0101;

    pub const fn zeroed() -> Self {
        Self {
            x: [0; 31],
            sp: 0,
            elr: 0,
            spsr: 0,
        }
    }
}

const _: [(); TrapFrame::SIZE_BYTES] = [(); core::mem::size_of::<TrapFrame>()];
const _: [(); 34] = [(); TrapFrame::WORDS];
const _: [(); TrapFrame::OFFSET_X0] = [(); core::mem::offset_of!(TrapFrame, x)];
const _: [(); 30 * 8] = [(); TrapFrame::OFFSET_X30];
const _: [(); TrapFrame::OFFSET_SP] = [(); core::mem::offset_of!(TrapFrame, sp)];
const _: [(); TrapFrame::OFFSET_ELR] = [(); core::mem::offset_of!(TrapFrame, elr)];
const _: [(); TrapFrame::OFFSET_SPSR] = [(); core::mem::offset_of!(TrapFrame, spsr)];
