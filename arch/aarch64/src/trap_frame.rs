#[repr(C)]
#[derive(Copy, Clone)]
pub struct TrapFrame {
    pub x: [u64; 31],
    /// Resume stack pointer.
    ///
    /// - EL1 frame: restored into the current EL1 `sp`.
    /// - EL0 frame: restored into `SP_EL0` as the user stack pointer.
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
    /// Valid EL1 stack pointer for the thread represented by this frame.
    ///
    /// For EL1 frames this is equal to `sp`. For EL0 frames this is the kernel
    /// stack to install before `eret`, while `sp` remains the user stack.
    pub kernel_sp: u64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FrameMode {
    KernelEl1,
    UserEl0,
    Unsupported,
}

impl TrapFrame {
    // Assembly contract (`exceptions.s`) depends on these offsets and size.
    pub const OFFSET_X0: usize = 0;
    pub const OFFSET_X30: usize = 30 * 8;
    pub const OFFSET_SP: usize = 31 * 8;
    pub const OFFSET_ELR: usize = 32 * 8;
    pub const OFFSET_SPSR: usize = 33 * 8;
    pub const OFFSET_KERNEL_SP: usize = 34 * 8;
    pub const SIZE_BYTES: usize = 35 * 8;
    pub const STACK_SIZE_BYTES: usize = 36 * 8;
    pub const WORDS: usize = 35;

    pub const SPSR_MODE_MASK: u64 = 0b1111;
    pub const SPSR_MODE_EL0T: u64 = 0b0000;
    pub const SPSR_MODE_EL1H: u64 = 0b0101;

    // Keep interrupts unmasked in resumed threads for the current kernel stage.
    pub const SPSR_DAIF_UNMASKED: u64 = 0;
    pub const EL0T: u64 = Self::SPSR_MODE_EL0T | Self::SPSR_DAIF_UNMASKED;
    pub const EL1H: u64 = Self::SPSR_MODE_EL1H | Self::SPSR_DAIF_UNMASKED;

    pub const fn zeroed() -> Self {
        Self {
            x: [0; 31],
            sp: 0,
            elr: 0,
            spsr: 0,
            kernel_sp: 0,
        }
    }

    pub fn init_kernel_el1(
        &mut self,
        kernel_entry: usize,
        kernel_sp: usize,
        arg0: usize,
        arg1: usize,
    ) {
        *self = Self::zeroed();
        let kernel_sp = align_stack(kernel_sp) as u64;
        self.x[0] = arg0 as u64;
        self.x[1] = arg1 as u64;
        self.sp = kernel_sp;
        self.kernel_sp = kernel_sp;
        self.elr = kernel_entry as u64;
        self.spsr = Self::EL1H;
    }

    pub fn init_user_el0(
        &mut self,
        user_entry: usize,
        user_sp: usize,
        kernel_sp: usize,
        arg0: usize,
    ) {
        *self = Self::zeroed();
        self.x[0] = arg0 as u64;
        self.sp = align_stack(user_sp) as u64;
        self.kernel_sp = align_stack(kernel_sp) as u64;
        self.elr = user_entry as u64;
        self.spsr = Self::EL0T;
    }

    pub const fn frame_mode(&self) -> FrameMode {
        match self.spsr & Self::SPSR_MODE_MASK {
            Self::SPSR_MODE_EL0T => FrameMode::UserEl0,
            Self::SPSR_MODE_EL1H => FrameMode::KernelEl1,
            _ => FrameMode::Unsupported,
        }
    }
}

const fn align_stack(sp: usize) -> usize {
    sp & !0xf
}

const _: [(); TrapFrame::SIZE_BYTES] = [(); core::mem::size_of::<TrapFrame>()];
const _: [(); 35] = [(); TrapFrame::WORDS];
const _: [(); TrapFrame::OFFSET_X0] = [(); core::mem::offset_of!(TrapFrame, x)];
const _: [(); 30 * 8] = [(); TrapFrame::OFFSET_X30];
const _: [(); TrapFrame::OFFSET_SP] = [(); core::mem::offset_of!(TrapFrame, sp)];
const _: [(); TrapFrame::OFFSET_ELR] = [(); core::mem::offset_of!(TrapFrame, elr)];
const _: [(); TrapFrame::OFFSET_SPSR] = [(); core::mem::offset_of!(TrapFrame, spsr)];
const _: [(); TrapFrame::OFFSET_KERNEL_SP] = [(); core::mem::offset_of!(TrapFrame, kernel_sp)];
