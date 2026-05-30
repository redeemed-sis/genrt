use crate::{process, sched};

pub(crate) fn kernel_init_thread(_arg: sched::ThreadArg) -> usize {
    crate::info!("init: scheduler is running");
    crate::info!("init: spawning first EL0 process");

    match process::spawn_first_user_process() {
        Ok(process) => match process::join(process) {
            Ok(code) => crate::info!("init: user process exited code={code}"),
            Err(err) => crate::error!("init: user process join failed: {err:?}"),
        },
        Err(err) => crate::error!("init: failed to spawn first user process: {err:?}"),
    }

    0
}
