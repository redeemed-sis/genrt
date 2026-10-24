//! Owned, restartable backend submissions. No backend callback may sleep.

use crate::{errno::Errno, sched::WaitToken};
use alloc::{boxed::Box, vec::Vec};
use core::{any::Any, task::Poll};

/// Maximum component size in bytes, keeping every dirent within bounded usercopy.
pub(crate) const NAME_MAX: usize = 255;

/// Minimum reusable staging capacity, allocated once on successful open.
pub(crate) const IO_CAPACITY: usize = 4096;

/// Backend-owned stable node identity; meaningful only within its instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    /// Instance-local node number, pinned until shutdown.
    pub(crate) id: u64,
    /// Node type, immutable for its lifetime.
    pub(crate) kind: Kind,
}

/// Supported namespace node types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    File,
    Directory,
}

/// Basic node metadata, not a userspace ABI structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stat {
    /// Stable node identity and kind.
    pub(crate) node: Node,
    /// Current byte size (zero is permitted for generated files).
    pub(crate) size: u64,
}

/// Owned immediate-child directory metadata.
pub(crate) struct DirEntry {
    /// Nonempty child component, at most NAME_MAX bytes, without slash or NUL.
    pub(crate) name: Vec<u8>,
    /// Instance-local node identity.
    pub(crate) node: Node,
}

/// Validated access and creation policy, independent of syscall encoding.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OpenFlags {
    /// Permit sequential reads.
    pub(crate) read: bool,
    /// Permit sequential writes.
    pub(crate) write: bool,
    /// Create a missing final regular-file component.
    pub(crate) create: bool,
    /// Truncate after successful open.
    pub(crate) truncate: bool,
    /// Select EOF and perform each write atomically across all opens.
    pub(crate) append: bool,
    /// Require a directory target.
    pub(crate) directory: bool,
}

/// One backend operation. Inputs remain stable across `Pending` resumes.
#[allow(dead_code)] // Seek/sync/stat are kernel backend APIs, not new syscalls.
pub(crate) enum Operation {
    Lookup(Vec<u8>),
    Create(Vec<u8>),
    Open(OpenFlags),
    Read {
        offset: u64,
        limit: usize,
    },
    Write {
        offset: u64,
        append: bool,
    },
    Readdir {
        offset: u64,
    },
    Truncate {
        size: u64,
    },
    Seek {
        current: u64,
        displacement: i64,
        whence: u8,
    },
    Sync,
    Stat,
}

/// Backend completion, separate from its private open and continuation state.
#[allow(dead_code)] // Metadata completion is consumed by kernel callers/tests.
pub(crate) enum Reply {
    Node(Node),
    Done,
    /// Byte count and actual next position, including atomic append selection.
    Io {
        count: usize,
        next: u64,
    },
    Entry(Option<DirEntry>),
    Position(u64),
    Stat(Stat),
}

/// Retained submission, never borrowed from a syscall stack across handoff.
pub(crate) struct Request {
    /// Target node (parent for lookup/create).
    pub(crate) node: Node,
    /// Immutable request inputs.
    pub(crate) operation: Operation,
    /// Owned bounded I/O staging; read fills it, write consumes its bytes.
    pub(crate) buffer: Vec<u8>,
    /// Backend-specific per-open state, exclusively leased from the OFD.
    pub(crate) open: Option<Box<dyn Any + Send>>,
    /// Backend continuation, or none when reusable progress lives in `open`.
    /// `Pending` must retain submitted effects in one of these owned resources
    /// and resume them exactly once. Read/write resources are prepared on open.
    #[allow(dead_code)] // Used by deferred backends; production ramfs is synchronous.
    pub(crate) continuation: Option<Box<dyn Any + Send>>,
}

impl Request {
    /// Construct an unsubmitted request; may subsequently allocate in task work.
    /// # Arguments
    /// * `node` - Stable target node.
    /// * `operation` - Operation with owned inputs.
    /// # Returns
    /// Empty staging/private state; no allocation or IRQ changes.
    pub(crate) fn new(node: Node, operation: Operation) -> Self {
        Self {
            node,
            operation,
            buffer: Vec::new(),
            open: None,
            continuation: None,
        }
    }
}

/// Filesystem instance contract. Every callback is finite and nonblocking.
///
/// Calls run outside process, scheduler, and OFD locks. Runtime task work has
/// IRQs enabled and preemption excluded only to protect a live EL0 frame.
/// Exclusive bootstrap traversal is synchronous and may run with IRQs masked.
/// Future drivers submit work, return `Pending`, and complete the exact token
/// after publishing result storage. No user pointers may be retained.
pub(crate) trait Backend: Send + Sync {
    /// Return the immutable instance root.
    /// # Returns
    /// A pinned directory node, without allocation or blocking.
    fn root(&self) -> Node;

    /// Submit or resume one owned operation; must not sleep. Open must prepare
    /// private/continuation resources needed by subsequent allocation-free I/O.
    /// Read/write may grow retained storage only beyond IO_CAPACITY when its
    /// existing capacity is insufficient; they must not allocate ephemeral state.
    /// # Arguments
    /// * `request` - Exclusive retained request, including prior continuation.
    /// * `wake` - Exact prepared wait to publish before returning `Pending`.
    ///   `None` permits synchronous bootstrap/host calls only.
    /// # Returns
    /// `Ready` with one completion, or `Pending` only after arranging an exact
    /// token completion. A resume may use a fresh token if still incomplete.
    /// # Errors
    /// Backend errno, including `ENOTSUP` for unsupported operations. Without
    /// `wake`, operations that need a wait must return `ENOTSUP`, not `Pending`.
    fn poll(&self, request: &mut Request, wake: Option<WaitToken>) -> Poll<Result<Reply, Errno>>;

    /// Reset completed or abandoned operation state before publishing its lease.
    /// # Arguments
    /// * `_request` - Exclusively owned request; preserve its open resources.
    /// # Returns
    /// Nothing. Runs outside owner locks, without blocking or allocating.
    /// Read/write resources must be retained for reuse, including cancellation.
    fn reset(&self, _request: &mut Request) {}

    /// Release final open state; must neither block nor allocate unbounded work.
    /// Durable flushing belongs to explicit `Sync`, not destructor context.
    /// # Arguments
    /// * `node` - Pinned node previously opened by this instance.
    /// * `state` - Exclusive final private open state, or none on partial open.
    /// # Returns
    /// Nothing; runs with IRQs enabled, outside all shared owner locks.
    fn close(&self, node: Node, state: Option<Box<dyn Any + Send>>);

    /// Borrow immutable executable bytes where this instance supports it.
    /// # Arguments
    /// * `_node` - Resolved executable target.
    /// # Returns
    /// Archive-pinned executable bytes, or none for unsupported backends.
    /// Does not allocate or block; ordinary reads do not use this shortcut.
    fn executable(&self, _node: Node) -> Option<&'static [u8]> {
        None
    }
}
