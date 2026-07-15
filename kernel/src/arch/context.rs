use core::{ffi::c_void, marker::PhantomData, ptr::NonNull};

unsafe extern "C" {
    fn arch_active_context_set_syscall_result(frame: *mut c_void, value: isize);
    fn arch_active_context_restart_syscall(frame: *mut c_void);
    fn arch_active_context_replace_user(frame: *mut c_void, user_entry: usize, user_sp: usize);
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
    /// frame and remain live and exclusively borrowed for `'a`. Its layout and
    /// alignment must also match the saved-frame word ABI used by the temporary
    /// scheduler migration bridge. The caller must not create another context
    /// or access the frame directly until the returned value is dropped.
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

    /// Expose the temporary saved-frame word bridge to scheduler handoff code.
    ///
    /// # Returns
    ///
    /// Returns a non-null pointer to the first word of the live frame. Only the
    /// low-level scheduler saved-frame copy and fork-clone paths may consume it.
    /// The pointer remains covered by this context's exclusive borrow and must
    /// not be stored. This operation does not allocate, block, or alter IRQ
    /// state.
    pub(crate) fn scheduler_frame_words(&mut self) -> NonNull<u64> {
        self.frame.cast()
    }
}

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
