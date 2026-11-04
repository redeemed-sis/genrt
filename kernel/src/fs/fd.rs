//! Bounded descriptors, shared OFDs, and lock-free backend request ownership.

use super::{
    backend::{IO_CAPACITY, Kind, OpenFlags, Operation, Reply, Request},
    io::Descriptor,
    vfs::{Location, Namespace, Resolver},
};
use crate::{
    config::KERNEL_THREAD_CAPACITY,
    errno::{self, Errno},
    sched::{self, WaitCause, WaitToken},
    sync::SpinLock,
};
use alloc::{boxed::Box, sync::Arc};
use core::task::Poll;

pub(crate) const MAX_FDS: usize = 32;

struct OpenState {
    offset: u64,
    request: Request,
}

struct DescriptionState {
    available: Option<Box<OpenState>>,
    waiters: [Option<WaitToken>; KERNEL_THREAD_CAPACITY],
}

/// Reference-counted open description. Fork shares this exact offset/private state.
/// Its short lock only transfers ownership; callbacks and usercopy run unlocked.
pub(crate) struct OpenDescription {
    location: Location,
    flags: OpenFlags,
    state: SpinLock<DescriptionState>,
}

impl Drop for OpenDescription {
    fn drop(&mut self) {
        // Final Arc cannot disappear while a transaction retains its own Arc.
        let mut state = self
            .state
            .lock()
            .available
            .take()
            .expect("vfs: final in-flight OFD");
        self.location
            .backend()
            .close(self.location.node(), state.request.open.take());
    }
}

#[cfg(test)]
impl OpenDescription {
    /// Inspect and remove one exact registration without a live host scheduler.
    /// # Arguments
    /// * `index` - Synthetic host thread slot within the bounded waiter array.
    /// # Returns
    /// Retained token or none, without allocation or backend callbacks. QEMU
    /// contracts exercise actual scheduler completion after lease publication.
    /// # Panics
    /// Panics for an out-of-range synthetic slot.
    pub(crate) fn take_waiter_for_test(&self, index: usize) -> Option<WaitToken> {
        self.state.lock().waiters[index].take()
    }
}

/// Fixed-size descriptor table. Cloning only increments shared references.
#[derive(Clone)]
pub(crate) struct FdTable {
    entries: [Option<Arc<Descriptor>>; MAX_FDS],
}

impl FdTable {
    /// Check whether no descriptor references remain.
    /// # Returns
    /// True for an empty table; bounded and allocation-free, without callbacks.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.iter().all(Option::is_none)
    }
    /// Inspect the next available slot before any create/truncate side effect.
    /// # Returns
    /// Lowest free slot including stdio, without allocation or callbacks.
    /// # Errors
    /// `EMFILE` if all slots are occupied.
    pub(crate) fn next_free(&self) -> Result<usize, Errno> {
        (0..MAX_FDS)
            .find(|i| self.entries[*i].is_none())
            .ok_or(errno::EMFILE)
    }
    /// Construct an empty table, allocation-free.
    /// # Returns
    /// Empty table, including slots 0/1/2. Initial stdio is staged separately.
    pub(crate) const fn new() -> Self {
        Self {
            entries: [const { None }; MAX_FDS],
        }
    }

    /// Move a descriptor into the lowest free slot without destruction.
    /// # Arguments
    /// * `description` - Taken only on success, left owned by caller on failure.
    /// # Returns
    /// Lowest free descriptor; no allocation or endpoint/backend calls.
    /// # Errors
    /// `EMFILE` if full, `EBADF` if no input description.
    pub(crate) fn install(
        &mut self,
        description: &mut Option<Arc<Descriptor>>,
    ) -> Result<usize, Errno> {
        let fd = self.next_free()?;
        if description.is_none() {
            return Err(errno::EBADF);
        }
        self.entries[fd] = description.take();
        Ok(fd)
    }

    /// Clone one descriptor under bounded table ownership.
    /// # Arguments
    /// * `fd` - Descriptor, including stdio.
    /// # Returns
    /// Shared VFS/endpoint target without allocation or callbacks.
    /// # Errors
    /// `EBADF` for closed or out-of-range descriptors.
    pub(crate) fn get(&self, fd: usize) -> Result<Arc<Descriptor>, Errno> {
        self.entries
            .get(fd)
            .and_then(Clone::clone)
            .ok_or(errno::EBADF)
    }

    /// Extract ownership for destruction after releasing the process lock.
    /// # Arguments
    /// * `fd` - Descriptor to remove, including stdio.
    /// # Returns
    /// Removed target reference; no destruction or callbacks occur here.
    /// # Errors
    /// `EBADF` for invalid or closed descriptors.
    pub(crate) fn take(&mut self, fd: usize) -> Result<Arc<Descriptor>, Errno> {
        self.entries
            .get_mut(fd)
            .and_then(Option::take)
            .ok_or(errno::EBADF)
    }

    /// Duplicate a reference into the lowest free slot, without allocation.
    /// # Arguments
    /// * `fd` - Existing descriptor to share.
    /// # Returns
    /// New lowest descriptor; no callbacks or final destruction.
    /// # Errors
    /// `EBADF` for invalid source, `EMFILE` for an exhausted table.
    pub(crate) fn dup(&mut self, fd: usize) -> Result<usize, Errno> {
        // Validate before cloning so failure cannot drop a reference under lock.
        if self.entries.get(fd).is_none_or(Option::is_none) {
            return Err(errno::EBADF);
        }
        let next = self.next_free()?;
        self.entries[next] = self.entries[fd].clone();
        Ok(next)
    }

    /// Atomically duplicate into a chosen slot, moving replaced ownership out.
    /// # Arguments
    /// * `fd` - Existing source descriptor, validated even for self replacement.
    /// * `target` - Destination slot, including stdio.
    /// # Returns
    /// Replaced reference for destruction outside the process lock, or none for
    /// an empty destination or valid self replacement. No allocation/callbacks.
    /// # Errors
    /// `EBADF` for invalid source or out-of-range destination; leaves both intact.
    pub(crate) fn dup2(
        &mut self,
        fd: usize,
        target: usize,
    ) -> Result<Option<Arc<Descriptor>>, Errno> {
        if target >= MAX_FDS || self.entries.get(fd).is_none_or(Option::is_none) {
            return Err(errno::EBADF);
        }
        if fd == target {
            return Ok(None);
        }
        let replacement = self.entries[fd].clone();
        Ok(core::mem::replace(&mut self.entries[target], replacement))
    }
}

impl Default for FdTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Restartable resolve/open/truncate transaction. Each stage advances once.
pub(crate) struct Open {
    resolver: Resolver,
    location: Option<Location>,
    request: Option<Request>,
    flags: OpenFlags,
    opened: bool,
    truncated: bool,
}

impl Open {
    /// Start an open with owned path inputs in ordinary task context.
    /// # Arguments
    /// * `cwd` - Pinned cwd.
    /// * `path` - Original bounded path.
    /// * `flags` - Validated access policy.
    /// # Returns
    /// Owned transaction; may allocate, does not block or touch process state.
    /// # Errors
    /// Path validation errors, or `EINVAL` for truncate without write access
    /// or create combined with directory-only access.
    pub(crate) fn new(cwd: Location, path: &[u8], flags: OpenFlags) -> Result<Self, Errno> {
        if flags.truncate && !flags.write {
            return Err(errno::EINVAL);
        }
        if flags.create && flags.directory {
            return Err(errno::EINVAL);
        }
        Ok(Self {
            resolver: Resolver::new(cwd, path, flags.create)?,
            location: None,
            request: None,
            flags,
            opened: false,
            truncated: false,
        })
    }

    /// Advance the exact resolve/open/truncate stages without sleeping.
    /// # Arguments
    /// * `namespace` - Frozen mount namespace.
    /// * `wake` - Prepared wait token for a backend pending stage.
    /// # Returns
    /// New independently positioned OFD, or pending with state retained here.
    /// # Errors
    /// Path/backend errors; `ENOTDIR` for directory-only access to a file,
    /// `EISDIR` for writable directory opens, `ENOMEM` if reusable staging cannot
    /// be reserved. May allocate in task work; failed reservation retains cleanup.
    pub(crate) fn poll(
        &mut self,
        namespace: &Namespace,
        wake: Option<WaitToken>,
    ) -> Poll<Result<Arc<OpenDescription>, Errno>> {
        if self.location.is_none() {
            self.location = Some(match self.resolver.poll(namespace, wake) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(r) => r?,
            });
        }
        let location = self.location.as_ref().unwrap();
        if self.flags.directory && location.node().kind != Kind::Directory {
            return Poll::Ready(Err(errno::ENOTDIR));
        }
        if self.flags.write && location.node().kind == Kind::Directory {
            return Poll::Ready(Err(errno::EISDIR));
        }
        if !self.opened {
            let request = self
                .request
                .get_or_insert_with(|| Request::new(Operation::Open(self.flags)));
            match location.poll(request, wake) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(Reply::Done)) => self.opened = true,
                Poll::Ready(Ok(_)) => return Poll::Ready(Err(errno::EIO)),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            }
        }
        if self.flags.truncate && !self.truncated {
            let request = self.request.as_mut().unwrap();
            if !matches!(request.operation, Operation::Truncate { .. }) {
                location.backend().reset(request);
                request.operation = Operation::Truncate { size: 0 };
            }
            match location.poll(request, wake) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(Reply::Done)) => self.truncated = true,
                Poll::Ready(Ok(_)) => return Poll::Ready(Err(errno::EIO)),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            }
        }
        let request = self.request.as_mut().unwrap();
        location.backend().reset(request);
        request.operation = Operation::Sync;
        request.buffer.clear();
        if request.buffer.capacity() < IO_CAPACITY {
            request
                .buffer
                .try_reserve_exact(IO_CAPACITY)
                .map_err(|_| errno::ENOMEM)?;
        }
        let request = self.request.take().unwrap();
        Poll::Ready(Ok(Arc::new(OpenDescription {
            location: location.clone(),
            flags: self.flags,
            state: SpinLock::new(DescriptionState {
                available: Some(Box::new(OpenState { offset: 0, request })),
                waiters: [None; KERNEL_THREAD_CAPACITY],
            }),
        })))
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        if let (Some(location), Some(request)) = (&self.location, &mut self.request) {
            // Includes partial-open failure; close must tolerate absent state.
            location
                .backend()
                .close(location.node(), request.open.take());
        }
    }
}

/// Exclusive logical OFD lease, not a held lock. Retained across scheduler waits.
/// Offset changes commit only after successful usercopy via `commit`.
pub(crate) struct Transaction {
    description: Arc<OpenDescription>,
    operation: Option<Operation>,
    length: usize,
    lease: Option<Box<OpenState>>,
    next: Option<u64>,
    copied: bool,
}

const _: () = assert!(core::mem::size_of::<Transaction>() <= 128);

impl Transaction {
    /// Validate a small operation intent without acquiring or allocating staging.
    /// # Arguments
    /// * `description` - Shared OFD to serialize.
    /// * `operation` - Read/write/readdir/seek/sync/stat operation.
    /// * `length` - Staging length; syscall callers retain their usercopy clamp.
    /// # Returns
    /// Unsubmitted transaction, without acquiring any owner lock.
    /// # Errors
    /// `EBADF` for disallowed access, `EISDIR`/`ENOTDIR` for wrong node type,
    /// `EINVAL` for non-FD operations. No allocation, callbacks, or blocking.
    pub(crate) fn new(
        description: Arc<OpenDescription>,
        operation: Operation,
        length: usize,
    ) -> Result<Self, Errno> {
        let kind = description.location.node().kind;
        match operation {
            Operation::Read { .. } => {
                if !description.flags.read {
                    return Err(errno::EBADF);
                }
                if kind == Kind::Directory {
                    return Err(errno::EISDIR);
                }
            }
            Operation::Write { .. } => {
                if !description.flags.write {
                    return Err(errno::EBADF);
                }
                if kind == Kind::Directory {
                    return Err(errno::EISDIR);
                }
            }
            Operation::Readdir { .. } => {
                if !description.flags.read {
                    return Err(errno::EBADF);
                }
                if kind != Kind::Directory {
                    return Err(errno::ENOTDIR);
                }
            }
            Operation::Truncate { .. } => {
                if !description.flags.write {
                    return Err(errno::EBADF);
                }
                if kind == Kind::Directory {
                    return Err(errno::EISDIR);
                }
            }
            Operation::Seek { .. } | Operation::Sync | Operation::Stat => {}
            _ => return Err(errno::EINVAL),
        }
        Ok(Self {
            description,
            operation: Some(operation),
            length,
            lease: None,
            next: None,
            copied: false,
        })
    }

    fn empty_io(&self) -> bool {
        self.length == 0
            && matches!(
                self.operation,
                Some(Operation::Read { .. } | Operation::Write { .. })
            )
    }

    /// Acquire reusable staging, then copy write input once before submission.
    /// # Arguments
    /// * `wake` - Exact prepared token registered if another call owns the OFD.
    /// * `input` - Write usercopy, invoked only after acquisition and never again
    ///   after success, including across backend Pending/resume.
    /// # Returns
    /// Ready after preparation (also for zero I/O without acquisition), or Pending
    /// without touching busy staging. No locks span growth or input copying.
    /// # Errors
    /// `EAGAIN` for busy synchronous calls, `ENOMEM` on growth, or input errno.
    /// Only lengths above IO_CAPACITY may grow retained staging; it never shrinks.
    /// Drop the transaction after an error; its lease remains owned for cleanup.
    pub(crate) fn prepare(
        &mut self,
        wake: Option<WaitToken>,
        input: impl FnOnce(&mut [u8]) -> Result<(), Errno>,
    ) -> Poll<Result<(), Errno>> {
        if self.empty_io() {
            return Poll::Ready(Ok(()));
        }
        if self.lease.is_none() {
            self.lease = {
                let mut state = self.description.state.lock();
                match state.available.take() {
                    Some(state) => Some(state),
                    None => {
                        let Some(token) = wake else {
                            return Poll::Ready(Err(errno::EAGAIN));
                        };
                        state.waiters[token.thread().index()] = Some(token);
                        None
                    }
                }
            };
            if self.lease.is_none() {
                return Poll::Pending;
            }
        }
        let state = self.lease.as_mut().unwrap();
        let request = &mut state.request;
        if self.operation.is_some() {
            if self.length > request.buffer.capacity() {
                assert!(self.length > IO_CAPACITY);
                request
                    .buffer
                    .try_reserve_exact(self.length - request.buffer.len())
                    .map_err(|_| errno::ENOMEM)?;
            }
            request.buffer.resize(self.length, 0);
            request.operation = self.operation.take().unwrap();
            match &mut request.operation {
                Operation::Read { offset, .. } | Operation::Readdir { offset } => {
                    *offset = state.offset;
                }
                Operation::Write { offset, append } => {
                    *offset = state.offset;
                    *append = self.description.flags.append;
                }
                Operation::Seek { current, .. } => *current = state.offset,
                _ => {}
            }
        }
        if matches!(request.operation, Operation::Write { .. }) && !self.copied {
            input(&mut request.buffer)?;
            self.copied = true;
        }
        Poll::Ready(Ok(()))
    }

    /// Submit/resume prepared I/O without retaining a lock.
    /// # Arguments
    /// * `wake` - Prepared token; busy OFDs publish it under their short lock.
    /// # Returns
    /// Backend completion or pending; no scheduler handoff occurs inside.
    /// # Errors
    /// Backend errors, `EAGAIN` for busy synchronous calls, `EIO` for invalid
    /// backend byte counts, `EINVAL` for write input not yet prepared.
    /// Read/write callbacks must reuse resources prepared on open.
    pub(crate) fn poll(&mut self, wake: Option<WaitToken>) -> Poll<Result<Reply, Errno>> {
        if self.empty_io() {
            return Poll::Ready(Ok(Reply::Io { count: 0, next: 0 }));
        }
        match self.prepare(wake, |_| Err(errno::EINVAL)) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result?,
        }
        let request = &mut self.lease.as_mut().unwrap().request;
        let result = self.description.location.poll(request, wake);
        if let Poll::Ready(Ok(reply)) = &result {
            self.next = match reply {
                Reply::Io { count, next } => {
                    if *count > request.buffer.len() {
                        return Poll::Ready(Err(errno::EIO));
                    }
                    Some(*next)
                }
                Reply::Entry(Some(_)) => match request.operation {
                    Operation::Readdir { offset } => offset.checked_add(1),
                    _ => None,
                },
                Reply::Position(next) => Some(*next),
                _ => self.next,
            };
        }
        result
    }

    /// Borrow owned staging after backend completion, outside all locks.
    /// # Returns
    /// Active staging, or empty without a lease. Only the completed byte count
    /// is output; the remaining initialized bytes may be from prior operations.
    /// No allocation or blocking.
    pub(crate) fn buffer(&self) -> &[u8] {
        self.lease.as_ref().map_or(&[], |s| &s.request.buffer)
    }

    /// Borrow leased output staging for directory-record encoding.
    /// # Returns
    /// Mutable bytes, without allocation, callbacks, or locking.
    /// # Panics
    /// Panics if preparation has not acquired a nonempty operation's lease.
    pub(crate) fn output_buffer(&mut self) -> &mut [u8] {
        &mut self
            .lease
            .as_mut()
            .expect("vfs: output without lease")
            .request
            .buffer
    }

    /// Inspect retained request and allocation identity in host contracts.
    /// # Returns
    /// Request address, staging address and capacity, or none without a lease.
    /// No allocation, callbacks, or locking.
    #[cfg(test)]
    pub(crate) fn staging_identity(&self) -> Option<(usize, usize, usize)> {
        self.lease.as_ref().map(|state| {
            (
                &state.request as *const Request as usize,
                state.request.buffer.as_ptr() as usize,
                state.request.buffer.capacity(),
            )
        })
    }

    /// Commit the staged position after successful copy; release ownership on drop.
    /// # Returns
    /// Nothing; only finite local metadata changes, no backend call.
    pub(crate) fn commit(&mut self) {
        if let Some(next) = self.next.take() {
            self.lease.as_mut().unwrap().offset = next;
        }
    }

    /// Query the staged next position after a backend completion.
    /// # Returns
    /// Next position or none for an operation without a position change.
    /// No allocation, blocking, or lock acquisition.
    pub(crate) fn next_offset(&self) -> Option<u64> {
        self.next
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        let Some(mut lease) = self.lease.take() else {
            return;
        };
        // Reset operation progress before publishing availability, retaining
        // initialized staging length and all reusable backend resources.
        // The retained request owns no Arc back to this description.
        self.description
            .location
            .backend()
            .reset(&mut lease.request);
        lease.request.operation = Operation::Sync;
        let waiters = {
            let mut state = self.description.state.lock();
            assert!(state.available.is_none());
            state.available = Some(lease);
            core::mem::replace(&mut state.waiters, [None; KERNEL_THREAD_CAPACITY])
        };
        for token in waiters.into_iter().flatten() {
            let _ = sched::complete_wait(token, WaitCause::Notified);
        }
    }
}
