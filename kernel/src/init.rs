use crate::{process, sched};

pub(crate) fn kernel_init_thread(_arg: sched::ThreadArg) -> usize {
    crate::info!("init: scheduler is running");
    crate::info!("init: spawning first EL0 process");

    match process::spawn_first_user_process() {
        Ok(pid) => match process::process_join(pid) {
            Ok(process::ProcessExitStatus::Exited(code)) => {
                crate::info!("init: user process exited code={code}");
            }
            Ok(process::ProcessExitStatus::Faulted(fault)) => {
                crate::warn!("init: user process faulted kind={:?}", fault.kind);
            }
            Err(err) => crate::error!("init: user process join failed: {err:?}"),
        },
        Err(err) => crate::error!("init: failed to spawn first user process: {err:?}"),
    }

    0
}
