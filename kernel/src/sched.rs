pub type TaskId = usize;
pub type Result<T> = core::result::Result<T, SchedError>;

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum TaskState {
    Ready,
    Running,
    Blocked,
}

#[derive(Copy, Clone)]
pub struct Task {
    pub id: TaskId,
    pub priority: u8,
    pub state: TaskState,
}

pub const MAX_TASKS: usize = 8;

const IDLE_TASK_ID: TaskId = 0;
const IDLE_PRIORITY: u8 = 0;

#[cfg(debug_assertions)]
const SCHED_LOG_EVERY: u64 = 101;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SchedError {
    InvalidTaskId,
    IdleTaskInvariant,
    AlreadyScheduled,
    NotScheduled,
}

pub struct Scheduler {
    tasks: [Task; MAX_TASKS],
    current: Option<TaskId>,
    initialized: bool,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            tasks: [
                Task {
                    id: 0,
                    priority: IDLE_PRIORITY,
                    state: TaskState::Blocked,
                },
                Task {
                    id: 1,
                    priority: 0,
                    state: TaskState::Blocked,
                },
                Task {
                    id: 2,
                    priority: 0,
                    state: TaskState::Blocked,
                },
                Task {
                    id: 3,
                    priority: 0,
                    state: TaskState::Blocked,
                },
                Task {
                    id: 4,
                    priority: 0,
                    state: TaskState::Blocked,
                },
                Task {
                    id: 5,
                    priority: 0,
                    state: TaskState::Blocked,
                },
                Task {
                    id: 6,
                    priority: 0,
                    state: TaskState::Blocked,
                },
                Task {
                    id: 7,
                    priority: 0,
                    state: TaskState::Blocked,
                },
            ],
            current: None,
            initialized: false,
        }
    }

    pub fn init(&mut self) {
        for task in &mut self.tasks {
            task.state = TaskState::Blocked;
            task.priority = 0;
        }

        self.tasks[IDLE_TASK_ID].priority = IDLE_PRIORITY;
        self.tasks[IDLE_TASK_ID].state = TaskState::Ready;
        self.current = None;
        self.initialized = true;
    }

    pub fn add_for_scheduling(&mut self, id: TaskId, priority: u8) -> Result<()> {
        if id >= MAX_TASKS {
            return Err(SchedError::InvalidTaskId);
        }
        if id == IDLE_TASK_ID {
            return Err(SchedError::IdleTaskInvariant);
        }
        if self.tasks[id].state != TaskState::Blocked {
            return Err(SchedError::AlreadyScheduled);
        }

        self.tasks[id].id = id;
        self.tasks[id].priority = priority;
        self.tasks[id].state = TaskState::Ready;
        Ok(())
    }

    pub fn remove_from_scheduling(&mut self, id: TaskId) -> Result<()> {
        if id >= MAX_TASKS {
            return Err(SchedError::InvalidTaskId);
        }
        if id == IDLE_TASK_ID {
            return Err(SchedError::IdleTaskInvariant);
        }
        if self.tasks[id].state == TaskState::Blocked {
            return Err(SchedError::NotScheduled);
        }

        self.tasks[id].state = TaskState::Blocked;
        self.tasks[id].priority = 0;

        if self.current == Some(id) {
            self.current = None;
            let _ = self.schedule();
        }

        Ok(())
    }

    pub fn on_tick(&mut self) {
        if !self.initialized {
            return;
        }

        let selected = self.schedule();

        #[cfg(debug_assertions)]
        if crate::time::ticks().is_multiple_of(SCHED_LOG_EVERY) {
            self.log_selected(selected);
        }
    }

    pub fn schedule(&mut self) -> TaskId {
        // Idle contract: task 0 must always remain selectable.
        self.tasks[IDLE_TASK_ID].state = TaskState::Ready;

        let selected = self.pick_next().unwrap_or(IDLE_TASK_ID);

        if self.current == Some(selected) {
            return selected;
        }

        if let Some(prev) = self.current
            && self.tasks[prev].state == TaskState::Running
        {
            self.tasks[prev].state = TaskState::Ready;
        }

        self.tasks[selected].state = TaskState::Running;
        self.current = Some(selected);
        selected
    }

    fn pick_next(&self) -> Option<TaskId> {
        let mut best: Option<TaskId> = None;

        for task in &self.tasks {
            if task.state != TaskState::Ready {
                continue;
            }

            match best {
                None => best = Some(task.id),
                Some(best_id) => {
                    let best_task = &self.tasks[best_id];
                    if task.priority > best_task.priority
                        || (task.priority == best_task.priority && task.id < best_task.id)
                    {
                        best = Some(task.id);
                    }
                }
            }
        }

        best
    }

    #[cfg(debug_assertions)]
    fn log_selected(&self, selected: TaskId) {
        crate::console::puts("[sched] current=");
        crate::debug::put_usize_dec(selected);
        crate::console::puts(" prio=");
        crate::debug::put_usize_dec(self.tasks[selected].priority as usize);
        crate::console::puts("\r\n");
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
