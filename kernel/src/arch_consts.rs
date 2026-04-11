#[cfg(target_arch = "aarch64")]
pub const TASK_FRAME_WORDS: usize = 34;

#[cfg(not(target_arch = "aarch64"))]
compile_error!("TASK_FRAME_WORDS is not defined for this architecture yet");
