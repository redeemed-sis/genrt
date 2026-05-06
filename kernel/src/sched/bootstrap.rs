use alloc::{collections::VecDeque, vec::Vec};

use crate::{task::TaskId, time::TimeHandlers};

use super::{
    IDLE_TASK_ID, Result, SchedError, Scheduler, THREAD_STACK_SIZE, arch_init_thread_frame,
    ipc as sched_ipc, preempt, scheduler_slot_mut, thread,
};

#[derive(Copy, Clone)]
pub(crate) struct StaticTask {
    priority: u8,
    entry: thread::ThreadEntry,
    arg: thread::ThreadArg,
}

impl StaticTask {
    pub(crate) const fn new(
        priority: u8,
        entry: thread::ThreadEntry,
        arg: thread::ThreadArg,
    ) -> Self {
        Self {
            priority,
            entry,
            arg,
        }
    }
}

pub(crate) fn bootstrap(
    idle_entry: thread::ThreadEntry,
    idle_arg: thread::ThreadArg,
    tasks: &[StaticTask],
    rr_quantum_ms: u64,
    thread_capacity: usize,
) -> Result<()> {
    if scheduler_slot_mut().is_some() {
        return Err(SchedError::AlreadyBootstrapped);
    }

    // Bootstrap ordering is intentionally split into two phases:
    // 1. Build and publish the global scheduler so timer-owned callbacks always
    //    have a concrete scheduler instance to reach.
    // 2. Only then initialize `kernel::time`, which is the first point where
    //    timer IRQ dispatch can invoke scheduler callbacks.
    let scheduler =
        Scheduler::bootstrap_new(idle_entry, idle_arg, tasks, rr_quantum_ms, thread_capacity)?;
    let task_count = scheduler.tasks.len();
    *scheduler_slot_mut() = Some(scheduler);
    init_time_after_scheduler_publish(task_count);
    Ok(())
}

#[inline(always)]
fn init_time_after_scheduler_publish(task_count: usize) {
    crate::time::init(
        TimeHandlers {
            finish_timer_interrupt: preempt::finish_timer_interrupt,
            wake_task: preempt::on_wake_task,
            quantum_expired: preempt::on_quantum_expired,
            ipc_timeout: sched_ipc::on_ipc_timeout,
        },
        task_count.saturating_mul(3),
    );
}

impl Scheduler {
    fn bootstrap_new(
        idle_entry: thread::ThreadEntry,
        idle_arg: thread::ThreadArg,
        tasks: &[StaticTask],
        rr_quantum_ms: u64,
        thread_capacity: usize,
    ) -> Result<Self> {
        let bootstrap_task_count = tasks.len() + 1;
        if thread_capacity < bootstrap_task_count {
            return Err(SchedError::ThreadCapacityTooSmall);
        }

        let mut scheduler = Self::new(thread_capacity, rr_quantum_ms);
        scheduler.push_idle_task(idle_entry, idle_arg);
        for task in tasks {
            scheduler.push_bootstrap_task(task.priority, task.entry, task.arg);
        }
        scheduler.initialize_bootstrap_task_frames()?;
        scheduler.fill_free_slots(thread_capacity);

        let first = scheduler.dequeue_next_ready().unwrap_or(IDLE_TASK_ID);
        scheduler.mark_initial_running(first)?;
        crate::debug!(
            "sched: thread_capacity={} ready_queue_capacity={} stack_size={} quantum={}ms",
            scheduler.tasks.len(),
            scheduler.ready_queue.capacity(),
            THREAD_STACK_SIZE,
            scheduler.rr_quantum_ms
        );
        Ok(scheduler)
    }

    fn new(task_capacity: usize, rr_quantum_ms: u64) -> Self {
        let mut task_table = Vec::new();
        task_table.reserve_exact(task_capacity);

        let mut ready_queue = VecDeque::new();
        ready_queue.reserve_exact(task_capacity.saturating_sub(1));

        Self {
            tasks: task_table,
            ready_queue,
            current: None,
            rr_quantum_ms: rr_quantum_ms.max(1),
            resched_requested: false,
            entered_running_task: false,
        }
    }

    fn push_idle_task(&mut self, entry: thread::ThreadEntry, arg: thread::ThreadArg) {
        let id = TaskId::new(self.tasks.len());
        debug_assert_eq!(id, IDLE_TASK_ID);
        self.tasks.push(preempt::Task::bootstrap(
            entry,
            arg,
            preempt::TaskState::Ready,
            false,
        ));
    }

    fn push_bootstrap_task(
        &mut self,
        priority: u8,
        entry: thread::ThreadEntry,
        arg: thread::ThreadArg,
    ) {
        let id = TaskId::new(self.tasks.len());
        self.tasks.push(preempt::Task::bootstrap(
            entry,
            arg,
            preempt::TaskState::Ready,
            false,
        ));
        self.ready_push_back(id);
        crate::debug!("sched: bootstrap task {} priority={priority}", id);
    }

    fn fill_free_slots(&mut self, thread_capacity: usize) {
        while self.tasks.len() < thread_capacity {
            let id = TaskId::new(self.tasks.len());
            self.tasks.push(preempt::Task::free());
            crate::trace!("sched: prepared free thread slot {id}");
        }
    }

    fn initialize_bootstrap_task_frames(&mut self) -> Result<()> {
        for index in 0..self.tasks.len() {
            self.initialize_task_frame(TaskId::new(index))?;
        }
        Ok(())
    }

    fn initialize_task_frame(&mut self, id: TaskId) -> Result<()> {
        if !self.is_valid_task(id) {
            return Err(SchedError::InvalidTaskId);
        }

        let (stack_top, entry, arg) = {
            let task = self.task(id);
            (task.stack_top(), task.entry, task.arg.as_usize())
        };
        let frame_words = self.frame_words_mut_ptr(id);
        // SAFETY: scheduler owns frame storage; stack top, entry pointer, and
        // argument were fixed during bootstrap task-table setup.
        unsafe {
            arch_init_thread_frame(
                frame_words,
                stack_top,
                entry as *const () as usize,
                arg,
                thread::thread_entry_bootstrap as *const () as usize,
            );
        }
        Ok(())
    }
}
