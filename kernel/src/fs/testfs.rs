//! Opt-in contract fixture. Never linked into production builds.

use super::backend::*;
#[cfg(feature = "qemu-test-vfs")]
use super::vfs::Namespace;
use crate::{
    errno::{self, Errno},
    sched::{self, WaitCause, WaitToken},
    sync::SpinLock,
};
#[cfg(feature = "qemu-test-vfs")]
use alloc::sync::Arc;
use alloc::{boxed::Box, format, vec, vec::Vec};
use core::{
    any::Any,
    fmt::{self, Write},
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
};

struct File {
    parent: u64,
    name: Vec<u8>,
    data: Vec<u8>,
}
struct Store {
    files: Vec<File>,
}
struct State {
    store: Option<Store>,
    waiters: [Option<WaitToken>; crate::config::KERNEL_THREAD_CAPACITY],
}

struct Deferred {
    reply: Reply,
    stage: usize,
}

#[derive(Default)]
struct OpenPrivate {
    pending_read: bool,
    write_reply: Option<Reply>,
}

// Large enough for all decimal usize counters even on a 64-bit target.
struct Generated {
    bytes: [u8; 512],
    length: usize,
}

impl Write for Generated {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.length.checked_add(value.len()).ok_or(fmt::Error)?;
        let target = self.bytes.get_mut(self.length..end).ok_or(fmt::Error)?;
        target.copy_from_slice(value.as_bytes());
        self.length = end;
        Ok(())
    }
}

fn arm_delay(token: WaitToken) {
    #[cfg(not(test))]
    {
        let delay = crate::time::monotonic_duration_to_counts(0, 10_000_000).unwrap();
        crate::time::schedule_event(
            crate::time::now_counter() + delay,
            crate::time::TimedEvent::wait_deadline(token),
        );
    }
    #[cfg(test)]
    let _ = token;
}

#[cfg(not(test))]
fn assert_task_callback() {
    unsafe extern "C" {
        fn arch_irq_state_allows_sched_call(saved: u64) -> bool;
    }
    let guard = crate::sync::LocalIrqGuard::save_and_disable();
    // SAFETY: classify the opaque state returned by the paired architecture
    // IRQ hook; no architecture register encoding is interpreted here.
    let enabled = unsafe { arch_irq_state_allows_sched_call(guard.saved_state()) };
    drop(guard);
    assert!(enabled, "testfs: backend callback entered IRQ-masked");
    assert!(
        crate::sync::preempt::is_disabled(),
        "testfs: unprotected live frame"
    );
}

/// Independently owned writable fixture instance with generated files and delay.
pub(crate) struct TestFs {
    label: &'static str,
    state: SpinLock<State>,
    opens: AtomicUsize,
    closes: AtomicUsize,
    live: AtomicUsize,
    creates: AtomicUsize,
    pending: AtomicUsize,
    completed: AtomicUsize,
    deferred: [AtomicUsize; 4],
    resumed: [AtomicUsize; 4],
}

impl TestFs {
    /// Allocate fixture metadata outside shared locks.
    /// # Arguments
    /// * `label` - Static instance label appearing in generated content.
    /// # Returns
    /// Independent fixture with empty mutable root and one subdirectory seed.
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            label,
            state: SpinLock::new(State {
                store: Some(Store {
                    files: vec![File {
                        parent: 2,
                        name: b"seed".to_vec(),
                        data: format!("seed:{label}\n").into_bytes(),
                    }],
                }),
                waiters: [None; crate::config::KERNEL_THREAD_CAPACITY],
            }),
            opens: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            live: AtomicUsize::new(0),
            creates: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            deferred: core::array::from_fn(|_| AtomicUsize::new(0)),
            resumed: core::array::from_fn(|_| AtomicUsize::new(0)),
        }
    }

    /// Count actual create submissions for side-effect regression tests.
    /// # Returns
    /// Create count without allocation or owner locking.
    #[cfg(test)]
    pub(crate) fn creates(&self) -> usize {
        self.creates.load(Ordering::Relaxed)
    }

    /// Inspect exact quiescent release counters in focused host tests.
    /// # Returns
    /// Open, close and live counts; individual loads are diagnostic snapshots.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (usize, usize, usize) {
        (
            self.opens.load(Ordering::Relaxed),
            self.closes.load(Ordering::Relaxed),
            self.live.load(Ordering::Relaxed),
        )
    }

    /// Inspect exact deferred submissions and resumes after host fixture work.
    /// # Returns
    /// Stage counts (create/open/truncate/write); no allocation or locking.
    #[cfg(test)]
    pub(crate) fn deferred_counts(&self) -> ([usize; 4], [usize; 4]) {
        (
            self.deferred.each_ref().map(|v| v.load(Ordering::SeqCst)),
            self.resumed.each_ref().map(|v| v.load(Ordering::SeqCst)),
        )
    }

    fn access(
        &self,
        wake: Option<WaitToken>,
    ) -> Result<StoreLease<'_>, Poll<Result<Reply, Errno>>> {
        let store = {
            let mut state = self.state.lock();
            match state.store.take() {
                Some(store) => store,
                None => {
                    let Some(token) = wake else {
                        return Err(Poll::Ready(Err(errno::EAGAIN)));
                    };
                    state.waiters[token.thread().index()] = Some(token);
                    return Err(Poll::Pending);
                }
            }
        };
        Ok(StoreLease {
            fs: self,
            store: Some(store),
        })
    }
}

// Runtime allocation, parsing and byte copies use exclusive logical ownership,
// not a held SpinLock. This also serializes EOF selection+write across OFDs.
struct StoreLease<'a> {
    fs: &'a TestFs,
    store: Option<Store>,
}
impl Drop for StoreLease<'_> {
    fn drop(&mut self) {
        let waiters = {
            let mut state = self.fs.state.lock();
            assert!(state.store.is_none());
            state.store = self.store.take();
            core::mem::replace(
                &mut state.waiters,
                [None; crate::config::KERNEL_THREAD_CAPACITY],
            )
        };
        for token in waiters.into_iter().flatten() {
            let _ = sched::complete_wait(token, WaitCause::Notified);
        }
    }
}

fn node(id: u64) -> Node {
    Node {
        id,
        kind: if id <= 2 { Kind::Directory } else { Kind::File },
    }
}
const SPECIAL: [(&[u8], u64); 6] = [
    (b"sub", 2),
    (b"dynamic", 3),
    (b"pending", 4),
    (b"errors", 5),
    (b"open-error", 6),
    (b"stats", 7),
];

impl Backend for TestFs {
    fn root(&self) -> Node {
        node(1)
    }

    fn poll(&self, request: &mut Request, wake: Option<WaitToken>) -> Poll<Result<Reply, Errno>> {
        #[cfg(not(test))]
        if wake.is_some() {
            assert_task_callback();
        }
        if request
            .continuation
            .as_ref()
            .is_some_and(|c| c.is::<Deferred>())
        {
            let saved = request
                .continuation
                .take()
                .unwrap()
                .downcast::<Deferred>()
                .unwrap();
            self.resumed[saved.stage].fetch_add(1, Ordering::SeqCst);
            return Poll::Ready(Ok(saved.reply));
        }
        if matches!(
            request.operation,
            Operation::Read { .. } | Operation::Write { .. }
        ) {
            let private = request
                .open
                .as_mut()
                .unwrap()
                .downcast_mut::<OpenPrivate>()
                .unwrap();
            if let Some(reply) = private.write_reply.take() {
                self.resumed[3].fetch_add(1, Ordering::SeqCst);
                return Poll::Ready(Ok(reply));
            }
            if request.node.id == 4 && !private.pending_read {
                let Some(token) = wake else {
                    return Poll::Ready(Err(errno::ENOTSUP));
                };
                private.pending_read = true;
                self.pending.fetch_add(1, Ordering::SeqCst);
                arm_delay(token);
                return Poll::Pending;
            }
        }
        let mut lease = match self.access(wake) {
            Ok(lease) => lease,
            Err(result) => return result,
        };
        let store = lease.store.as_mut().unwrap();
        let id = request.node.id;
        let deferred_name = match &request.operation {
            Operation::Create(name) => name.starts_with(b"deferred-"),
            _ => id
                .checked_sub(16)
                .and_then(|i| store.files.get(i as usize))
                .is_some_and(|f| f.name.starts_with(b"deferred-")),
        };
        let stage = if deferred_name {
            match request.operation {
                Operation::Create(_) => Some(0),
                Operation::Open(_) => Some(1),
                Operation::Truncate { .. } => Some(2),
                Operation::Write { .. } => Some(3),
                _ => None,
            }
        } else {
            None
        };
        if stage.is_some() && wake.is_none() {
            return Poll::Ready(Err(errno::ENOTSUP));
        }
        let result = (|| match &request.operation {
            Operation::Lookup(name) | Operation::Create(name) => {
                if request.node.kind != Kind::Directory {
                    return Err(errno::ENOTDIR);
                }
                if id == 1
                    && let Some((_, id)) = SPECIAL.iter().find(|(n, _)| *n == name)
                {
                    return Ok(Reply::Node(node(*id)));
                }
                if let Some(i) = store
                    .files
                    .iter()
                    .position(|f| f.parent == id && f.name == *name)
                {
                    return Ok(Reply::Node(node(16 + i as u64)));
                }
                if matches!(request.operation, Operation::Lookup(_)) {
                    return Err(errno::ENOENT);
                }
                if name.is_empty() || name.contains(&b'/') || name.contains(&0) {
                    return Err(errno::EINVAL);
                }
                if name.len() > NAME_MAX {
                    return Err(errno::ENAMETOOLONG);
                }
                store.files.try_reserve(1).map_err(|_| errno::ENOMEM)?;
                let index = store.files.len();
                let mut data = Vec::new();
                data.try_reserve_exact(IO_CAPACITY)
                    .map_err(|_| errno::ENOMEM)?;
                store.files.push(File {
                    parent: id,
                    name: name.clone(),
                    data,
                });
                self.creates.fetch_add(1, Ordering::Relaxed);
                Ok(Reply::Node(node(16 + index as u64)))
            }
            Operation::Open(flags) => {
                self.opens.fetch_add(1, Ordering::Relaxed);
                self.live.fetch_add(1, Ordering::Relaxed);
                request.open = Some(Box::new(OpenPrivate::default()));
                if let Some(file) = id
                    .checked_sub(16)
                    .and_then(|i| store.files.get_mut(i as usize))
                    && file.data.capacity() < IO_CAPACITY
                {
                    file.data
                        .try_reserve_exact(IO_CAPACITY - file.data.len())
                        .map_err(|_| errno::ENOMEM)?;
                }
                if id == 6 {
                    return Err(errno::EIO);
                }
                if flags.write && id < 16 && id != 5 {
                    return Err(errno::EROFS);
                }
                Ok(Reply::Done)
            }
            Operation::Read { offset, limit } => {
                if id == 5 {
                    return Err(errno::EIO);
                }
                if request.node.kind == Kind::Directory {
                    return Err(errno::EISDIR);
                }
                let mut generated = Generated {
                    bytes: [0; 512],
                    length: 0,
                };
                match id {
                    3 => writeln!(generated, "dynamic:{}", self.label).map_err(|_| errno::EIO)?,
                    4 => writeln!(generated, "pending:{}", self.label).map_err(|_| errno::EIO)?,
                    7 => {
                        let opens = self.opens.load(Ordering::Relaxed);
                        let closes = self.closes.load(Ordering::Relaxed);
                        let live = self.live.load(Ordering::Relaxed);
                        let pending = self.pending.load(Ordering::SeqCst);
                        let completed = self.completed.load(Ordering::SeqCst);
                        let d = self.deferred.each_ref().map(|c| c.load(Ordering::SeqCst));
                        let r = self.resumed.each_ref().map(|c| c.load(Ordering::SeqCst));
                        writeln!(generated, "opens={opens} closes={closes} live={live} pending={pending} completed={completed} dc={} do={} dt={} dw={} rc={} ro={} rt={} rw={}", d[0], d[1], d[2], d[3], r[0], r[1], r[2], r[3]).map_err(|_| errno::EIO)?;
                    }
                    _ => {}
                }
                let bytes = if matches!(id, 3 | 4 | 7) {
                    &generated.bytes[..generated.length]
                } else {
                    &store
                        .files
                        .get(id.checked_sub(16).ok_or(errno::ENOENT)? as usize)
                        .ok_or(errno::ENOENT)?
                        .data
                };
                let start = usize::try_from(*offset)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let count = (*limit).min(request.buffer.len()).min(bytes.len() - start);
                request.buffer[..count].copy_from_slice(&bytes[start..start + count]);
                if id == 4 {
                    self.completed.fetch_add(1, Ordering::SeqCst);
                }
                Ok(Reply::Io {
                    count,
                    next: offset.checked_add(count as u64).ok_or(errno::EOVERFLOW)?,
                })
            }
            Operation::Write { offset, append } => {
                if id == 5 {
                    return Err(errno::EIO);
                }
                let file = store
                    .files
                    .get_mut(id.checked_sub(16).ok_or(errno::EROFS)? as usize)
                    .ok_or(errno::ENOENT)?;
                let start = if *append {
                    file.data.len()
                } else {
                    usize::try_from(*offset).map_err(|_| errno::EOVERFLOW)?
                };
                let end = start
                    .checked_add(request.buffer.len())
                    .ok_or(errno::EOVERFLOW)?;
                if end > file.data.capacity() {
                    assert!(end > IO_CAPACITY);
                    file.data
                        .try_reserve(end - file.data.len())
                        .map_err(|_| errno::ENOMEM)?;
                }
                if end > file.data.len() {
                    file.data.resize(end, 0);
                }
                file.data[start..end].copy_from_slice(&request.buffer);
                Ok(Reply::Io {
                    count: request.buffer.len(),
                    next: end as u64,
                })
            }
            Operation::Truncate { size } => {
                let file = store
                    .files
                    .get_mut(id.checked_sub(16).ok_or(errno::EROFS)? as usize)
                    .ok_or(errno::ENOENT)?;
                let size = usize::try_from(*size).map_err(|_| errno::EOVERFLOW)?;
                if size > file.data.len() {
                    file.data
                        .try_reserve(size - file.data.len())
                        .map_err(|_| errno::ENOMEM)?;
                }
                file.data.resize(size, 0);
                Ok(Reply::Done)
            }
            Operation::Readdir { offset } => {
                if request.node.kind != Kind::Directory {
                    return Err(errno::ENOTDIR);
                }
                let mut entries: Vec<DirEntry> = if id == 1 {
                    SPECIAL
                        .iter()
                        .map(|(name, id)| DirEntry {
                            name: name.to_vec(),
                            node: node(*id),
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                entries.extend(
                    store
                        .files
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| f.parent == id)
                        .map(|(i, f)| DirEntry {
                            name: f.name.clone(),
                            node: node(16 + i as u64),
                        }),
                );
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(Reply::Entry(
                    usize::try_from(*offset)
                        .ok()
                        .and_then(|i| entries.into_iter().nth(i)),
                ))
            }
            Operation::Stat | Operation::Seek { .. } => {
                let size = if id >= 16 {
                    store
                        .files
                        .get((id - 16) as usize)
                        .ok_or(errno::ENOENT)?
                        .data
                        .len() as u64
                } else {
                    0
                };
                if let Operation::Seek {
                    current,
                    displacement,
                    whence,
                } = request.operation
                {
                    let base = match whence {
                        0 => 0,
                        1 => current,
                        2 => size,
                        _ => return Err(errno::EINVAL),
                    };
                    Ok(Reply::Position(
                        base.checked_add_signed(displacement).ok_or(errno::EINVAL)?,
                    ))
                } else {
                    Ok(Reply::Stat(Stat {
                        node: request.node,
                        size,
                    }))
                }
            }
            Operation::Sync => {
                if id == 5 {
                    Err(errno::EIO)
                } else {
                    Ok(Reply::Done)
                }
            }
        })();
        // Preserve the completed result across the real scheduler restart. The
        // mutation and private-open allocation must never run again on resume.
        if let Some(stage) = stage
            && let Ok(reply) = result
        {
            if stage == 3 {
                request
                    .open
                    .as_mut()
                    .unwrap()
                    .downcast_mut::<OpenPrivate>()
                    .unwrap()
                    .write_reply = Some(reply);
            } else {
                request.continuation = Some(Box::new(Deferred { reply, stage }));
            }
            self.deferred[stage].fetch_add(1, Ordering::SeqCst);
            drop(lease);
            arm_delay(wake.unwrap());
            return Poll::Pending;
        }
        Poll::Ready(result)
    }

    fn close(&self, _node: Node, state: Option<Box<dyn Any + Send>>) {
        #[cfg(not(test))]
        assert_task_callback();
        if state.is_some() {
            self.live.fetch_sub(1, Ordering::Relaxed);
            self.closes.fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
    }

    fn reset(&self, request: &mut Request) {
        if let Some(private) = request
            .open
            .as_mut()
            .and_then(|s| s.downcast_mut::<OpenPrivate>())
        {
            private.pending_read = false;
            private.write_reply = None;
        }
        request.continuation = None;
    }
}

/// Install independent opt-in instances on archive-supplied existing directories.
/// # Arguments
/// * `namespace` - Exclusively owned boot namespace, not yet published.
/// # Returns
/// Success after pinning both fixtures; allocates during bootstrap only.
/// # Errors
/// Mount/path errors, including missing fixture directories.
#[cfg(feature = "qemu-test-vfs")]
pub(crate) fn install(namespace: &mut Namespace) -> Result<(), Errno> {
    namespace.mount(b"/.__genrt_test__/mount-a", Arc::new(TestFs::new("a")))?;
    namespace.mount(b"/.__genrt_test__/mount-b", Arc::new(TestFs::new("b")))
}
