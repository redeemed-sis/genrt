#[repr(C)]
#[derive(Copy, Clone)]
pub struct TrapFrame {
    pub x: [u64; 31],
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
}

impl TrapFrame {
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

const _: [(); 272] = [(); core::mem::size_of::<TrapFrame>()];
const _: [(); 34] = [(); TrapFrame::WORDS];
