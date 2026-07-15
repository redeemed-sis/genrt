use core::{ffi::c_void, marker::PhantomData, ptr::NonNull};

const SAVED_CONTEXT_PAYLOAD_BYTES: usize = 280;
const SAVED_CONTEXT_PADDING_BYTES: usize = 8;
const SAVED_CONTEXT_STORAGE_BYTES: usize =
    SAVED_CONTEXT_PAYLOAD_BYTES + SAVED_CONTEXT_PADDING_BYTES;
const SAVED_CONTEXT_STORAGE_ALIGN: usize = 16;

unsafe extern "C" {
    fn arch_active_context_set_syscall_result(frame: *mut c_void, value: isize);
    fn arch_active_context_restart_syscall(frame: *mut c_void);
    fn arch_active_context_replace_user(frame: *mut c_void, user_entry: usize, user_sp: usize);
    fn arch_saved_context_init_kernel(
        saved: *mut SavedContext,
        stack_top: usize,
        entry_addr: usize,
        arg: usize,
        bootstrap_pc: usize,
    );
    fn arch_saved_context_init_user(
        saved: *mut SavedContext,
        user_entry: usize,
        user_sp: usize,
        kernel_sp: usize,
        arg0: usize,
    );
    fn arch_saved_context_init_fork_child(
        saved: *mut SavedContext,
        active: *const c_void,
        child_kernel_sp: usize,
    );
    fn arch_saved_context_save(saved: *mut SavedContext, active: *const c_void);
    fn arch_saved_context_restore(saved: *const SavedContext, active: *mut c_void);
    fn arch_saved_context_enter(saved: *const SavedContext) -> !;
}

/// Exclusive access to the live architecture exception frame.
///
/// The frame representation is opaque to generic kernel code. The exclusive
/// lifetime prevents a live exception context from being copied or retained
/// beyond the architecture entry that owns the frame. Operations are bounded,
/// do not allocate, and delegate register details to the active architecture.
pub struct ActiveContext<'a> {
    frame: NonNull<c_void>,
    _exclusive: PhantomData<&'a mut c_void>,
}

impl<'a> ActiveContext<'a> {
    /// Wrap a live architecture exception frame.
    ///
    /// # Arguments
    ///
    /// * `frame` - Non-null opaque pointer to the live writable exception
    ///   frame owned by the architecture entry path.
    ///
    /// # Returns
    ///
    /// Returns an exclusive typed context tied to the caller-selected frame
    /// lifetime. The operation does not allocate, block, or alter IRQ state.
    ///
    /// # Safety
    ///
    /// `frame` must identify the active architecture's complete writable trap
    /// frame and remain live and exclusively borrowed for `'a`. The caller must
    /// not create another context or access the frame directly until the
    /// returned value is dropped.
    pub unsafe fn from_raw(frame: NonNull<c_void>) -> Self {
        Self {
            frame,
            _exclusive: PhantomData,
        }
    }

    /// Store the architecture syscall return value in the live context.
    ///
    /// # Arguments
    ///
    /// * `value` - Signed kernel syscall result, including negative errno
    ///   values.
    ///
    /// # Returns
    ///
    /// Returns no value. This operation is bounded and does not allocate,
    /// block, or alter IRQ state.
    pub(crate) fn set_syscall_result(&mut self, value: isize) {
        // SAFETY: construction guarantees that `frame` is the exclusively
        // borrowed live frame expected by the selected architecture hook.
        unsafe { arch_active_context_set_syscall_result(self.frame.as_ptr(), value) }
    }

    /// Rewind the live context so the current userspace syscall executes again.
    ///
    /// # Returns
    ///
    /// Returns no value. The architecture owns instruction-width and resume-PC
    /// semantics. This operation is bounded and does not allocate or block.
    ///
    /// # Panics
    ///
    /// The architecture hook may panic if the saved resume address cannot be
    /// rewound without underflow.
    pub(crate) fn restart_current_syscall(&mut self) {
        // SAFETY: construction guarantees exclusive access to a live frame.
        unsafe { arch_active_context_restart_syscall(self.frame.as_ptr()) }
    }

    /// Replace the EL0-visible state after committing a new process image.
    ///
    /// The architecture preserves the current thread's kernel exception stack
    /// while replacing userspace registers, PC, and SP. This operation is
    /// bounded and does not allocate, block, or alter IRQ state.
    ///
    /// # Arguments
    ///
    /// * `user_entry` - Entry virtual address in the newly active user address
    ///   space.
    /// * `user_sp` - Initial userspace stack pointer built for the new image.
    ///
    /// # Returns
    ///
    /// Returns no value.
    pub(crate) fn replace_user_context_after_exec(&mut self, user_entry: usize, user_sp: usize) {
        // SAFETY: construction guarantees exclusive access to a live frame;
        // process commit establishes the new address space before this call.
        unsafe {
            arch_active_context_replace_user(self.frame.as_ptr(), user_entry, user_sp);
        }
    }
}

/// Owned architecture context retained by one occupied scheduler slot.
///
/// The storage is inline, fully initialized, and deliberately opaque outside
/// the architecture facade. It is not `Copy` or `Clone`; moving the value
/// transfers ownership of the saved execution state. Construction and handoff
/// operations are bounded and allocation-free.
#[repr(C, align(16))]
pub struct SavedContext {
    payload: [u8; SAVED_CONTEXT_PAYLOAD_BYTES],
    alignment_tail: [u8; SAVED_CONTEXT_PADDING_BYTES],
}

impl SavedContext {
    /// Number of architecture payload bytes available at offset zero.
    ///
    /// # Returns
    ///
    /// This associated constant is the exact payload size that an architecture
    /// adapter must match with its saved frame.
    pub const PAYLOAD_BYTES: usize = SAVED_CONTEXT_PAYLOAD_BYTES;

    /// Total inline storage footprint, including explicit alignment tail bytes.
    ///
    /// # Returns
    ///
    /// This associated constant is the exact size of `SavedContext` in a task
    /// slot.
    pub const STORAGE_BYTES: usize = SAVED_CONTEXT_STORAGE_BYTES;

    /// Required alignment of scheduler-owned saved context storage.
    ///
    /// # Returns
    ///
    /// This associated constant is the exact alignment architecture adapters
    /// may rely on.
    pub const STORAGE_ALIGN: usize = SAVED_CONTEXT_STORAGE_ALIGN;

    /// Build the initial context for a kernel thread.
    ///
    /// # Arguments
    ///
    /// * `stack_top` - Exclusive upper bound of the thread's kernel stack.
    /// * `entry_addr` - Address of the kernel thread entry function.
    /// * `arg` - Raw [`crate::sched::ThreadArg`] payload for the entry function.
    /// * `bootstrap_pc` - Architecture return PC for the common thread bootstrap.
    ///
    /// # Returns
    ///
    /// Returns an owned kernel-entry context. The operation is bounded and does
    /// not allocate, block, or alter IRQ state.
    pub(crate) fn kernel_entry(
        stack_top: usize,
        entry_addr: usize,
        arg: usize,
        bootstrap_pc: usize,
    ) -> Self {
        let mut saved = Self::zeroed();
        // SAFETY: `saved` is aligned opaque storage owned exclusively by this
        // constructor. The architecture compile-time contract verifies that
        // its complete saved frame fits in the storage.
        unsafe {
            arch_saved_context_init_kernel(&mut saved, stack_top, entry_addr, arg, bootstrap_pc);
        }
        saved
    }

    /// Build the initial context for a userspace thread.
    ///
    /// # Arguments
    ///
    /// * `user_entry` - EL0 virtual address at which execution begins.
    /// * `user_sp` - Initial EL0 stack pointer.
    /// * `kernel_sp` - EL1 exception stack pointer owned by the thread.
    /// * `arg0` - Initial userspace argument register value.
    ///
    /// # Returns
    ///
    /// Returns an owned userspace-entry context. The operation is bounded and
    /// does not allocate, block, or alter IRQ state.
    pub(crate) fn user_entry(
        user_entry: usize,
        user_sp: usize,
        kernel_sp: usize,
        arg0: usize,
    ) -> Self {
        let mut saved = Self::zeroed();
        // SAFETY: the destination is exclusive, aligned, fully initialized
        // storage whose size is checked by the architecture adapter.
        unsafe {
            arch_saved_context_init_user(&mut saved, user_entry, user_sp, kernel_sp, arg0);
        }
        saved
    }

    /// Derive a fork child's saved userspace context from the live parent.
    ///
    /// # Arguments
    ///
    /// * `parent` - Exclusively borrowed live parent syscall context.
    /// * `child_kernel_sp` - EL1 exception stack pointer owned by the child.
    ///
    /// # Returns
    ///
    /// Returns an owned child context with architecture-defined fork return
    /// state. The operation is bounded and allocation-free.
    pub(crate) fn fork_child(parent: &mut ActiveContext<'_>, child_kernel_sp: usize) -> Self {
        let mut saved = Self::zeroed();
        // SAFETY: `parent` uniquely owns its live frame and `saved` uniquely
        // owns correctly sized/aligned destination storage.
        unsafe {
            arch_saved_context_init_fork_child(&mut saved, parent.frame.as_ptr(), child_kernel_sp);
        }
        saved
    }

    /// Save one live exception context into this scheduler-owned context.
    ///
    /// # Arguments
    ///
    /// * `active` - Exclusively borrowed live context being switched out.
    ///
    /// # Returns
    ///
    /// Returns no value. The operation performs a bounded architecture frame
    /// copy and does not allocate, block, or alter IRQ state.
    pub(crate) fn save_from(&mut self, active: &mut ActiveContext<'_>) {
        // SAFETY: both contexts are exclusively borrowed and represent distinct
        // architecture-owned live and scheduler-owned saved storage.
        unsafe { arch_saved_context_save(self, active.frame.as_ptr()) }
    }

    /// Restore this scheduler-owned context into a live exception context.
    ///
    /// # Arguments
    ///
    /// * `active` - Exclusively borrowed live context that will resume the task.
    ///
    /// # Returns
    ///
    /// Returns no value. The operation performs a bounded architecture frame
    /// copy and does not allocate, block, or alter IRQ state.
    pub(crate) fn restore_into(&self, active: &mut ActiveContext<'_>) {
        // SAFETY: `self` contains a typed, initialized saved frame and `active`
        // uniquely owns the writable live return frame.
        unsafe { arch_saved_context_restore(self, active.frame.as_ptr()) }
    }

    /// Enter this saved context as the scheduler's first running task.
    ///
    /// # Returns
    ///
    /// This function does not return. It transfers control to the architecture
    /// restore path. The operation is bounded and allocation-free.
    pub(crate) fn enter(&self) -> ! {
        // SAFETY: typed construction guarantees an initialized saved frame and
        // scheduler ownership keeps this inline storage stable while it runs.
        unsafe { arch_saved_context_enter(self) }
    }

    const fn zeroed() -> Self {
        Self {
            payload: [0; SAVED_CONTEXT_PAYLOAD_BYTES],
            alignment_tail: [0; SAVED_CONTEXT_PADDING_BYTES],
        }
    }
}

const _: () = {
    assert!(core::mem::size_of::<SavedContext>() == SAVED_CONTEXT_STORAGE_BYTES);
    assert!(core::mem::align_of::<SavedContext>() == SAVED_CONTEXT_STORAGE_ALIGN);
    assert!(core::mem::offset_of!(SavedContext, payload) == 0);
};

/// Architecture-neutral decoded userspace syscall request.
///
/// The architecture entry layer maps its syscall number and argument registers
/// into this fixed-width value before entering generic kernel dispatch.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SyscallRequest {
    number: usize,
    args: [usize; 6],
}

impl SyscallRequest {
    /// Construct a decoded syscall request.
    ///
    /// # Arguments
    ///
    /// * `number` - Architecture-decoded syscall number.
    /// * `args` - Six architecture-decoded syscall argument values in ABI
    ///   order.
    ///
    /// # Returns
    ///
    /// Returns an immutable fixed-width request. Construction does not
    /// allocate, block, or alter IRQ state.
    pub const fn new(number: usize, args: [usize; 6]) -> Self {
        Self { number, args }
    }

    /// Return the decoded syscall number.
    ///
    /// # Returns
    ///
    /// Returns the architecture-neutral syscall number. This accessor does not
    /// allocate, block, or alter IRQ state.
    pub const fn number(&self) -> usize {
        self.number
    }

    /// Return one decoded syscall argument.
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based ABI argument index in `0..6`.
    ///
    /// # Returns
    ///
    /// Returns the decoded argument at `index`. This accessor does not
    /// allocate, block, or alter IRQ state.
    ///
    /// # Panics
    ///
    /// Panics when `index` is outside `0..6`.
    pub const fn arg(&self, index: usize) -> usize {
        self.args[index]
    }

    /// Borrow all decoded syscall arguments in ABI order.
    ///
    /// # Returns
    ///
    /// Returns the fixed six-argument array. This accessor does not allocate,
    /// block, or alter IRQ state.
    pub const fn args(&self) -> &[usize; 6] {
        &self.args
    }
}
