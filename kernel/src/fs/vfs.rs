//! Bounded runtime namespace with pinned mounts and parent-aware locations.

use super::backend::{Backend, Kind, NAME_MAX, Node, Operation, Reply, Request};
use crate::{
    config::GENRT_PATH_MAX,
    errno::{self, Errno},
    sched::WaitToken,
    sync::{OncePublication, SpinLock},
};
use alloc::{sync::Arc, vec::Vec};
use core::task::Poll;

const MAX_MOUNTS: usize = 16;

/// A filesystem instance pinned by the namespace and every open location.
pub(crate) struct Mount {
    pub(super) backend: Arc<dyn Backend>,
    root: Node,
}

/// Pinned mount/node reference with immutable canonical namespace ancestry.
#[derive(Clone)]
pub(crate) struct Location(Arc<LocationInner>);

struct LocationInner {
    mount: Arc<Mount>,
    node: Node,
    absolute: Vec<u8>,
    parent: Option<Location>,
}

impl Drop for LocationInner {
    fn drop(&mut self) {
        // A bounded path may still contain thousands of components. Consume
        // uniquely owned ancestry iteratively instead of recursing on EL1's
        // bounded kernel stack. Shared suffixes remain owned by their peers.
        let mut parent = self.parent.take();
        while let Some(location) = parent {
            match Arc::try_unwrap(location.0) {
                Ok(mut inner) => parent = inner.parent.take(),
                Err(_) => break,
            }
        }
    }
}

impl Location {
    /// Query stable node identity and type.
    /// # Returns
    /// Instance-local identity; no allocation or blocking.
    pub(crate) fn node(&self) -> Node {
        self.0.node
    }

    /// Submit or resume an operation against this pinned mount and node.
    /// Retain this location across `Pending`; requests contain no target identity.
    /// # Arguments
    /// * `request` - Owned operation state, including any backend continuation.
    /// * `wake` - Exact prepared wait for a backend that cannot complete yet.
    /// # Returns
    /// Backend completion or `Pending` after arranging notification. Forwarding
    /// allocates nothing, holds no lock and changes no IRQ state; backend work
    /// follows the ordinary task/bootstrap callback contract.
    /// # Errors
    /// Propagates backend errors, including `ENOTSUP` for waits without a token.
    pub(crate) fn poll(
        &self,
        request: &mut Request,
        wake: Option<WaitToken>,
    ) -> Poll<Result<Reply, Errno>> {
        self.backend().poll(self.node(), request, wake)
    }

    /// Borrow the immutable canonical namespace path.
    /// # Returns
    /// Absolute path without trailing slash except root; no allocation.
    pub(crate) fn path(&self) -> &[u8] {
        &self.0.absolute
    }

    /// Obtain archive-pinned executable data after generic path traversal.
    /// # Returns
    /// Executable bytes or none for unsupported nodes; no allocation or blocking.
    pub(crate) fn executable(&self) -> Option<&'static [u8]> {
        self.0.mount.backend.executable(self.node())
    }

    pub(super) fn backend(&self) -> &dyn Backend {
        &*self.0.mount.backend
    }

    // Compare pinned identities, including self, without filesystem lookup.
    fn is_ancestor_of(&self, location: &Location) -> bool {
        let mut current = Some(location);
        while let Some(location) = current {
            if Arc::ptr_eq(&self.0.mount, &location.0.mount) && self.node() == location.node() {
                return true;
            }
            current = location.0.parent.as_ref();
        }
        false
    }
}

/// One namespace with permanent mounts and bounded synchronized publication.
pub(crate) struct Namespace {
    root: Location,
    mounts: SpinLock<MountTable>,
}

#[derive(Clone)]
struct MountTable {
    points: [Option<Arc<Mountpoint>>; MAX_MOUNTS],
    len: usize,
}

struct Mountpoint {
    covered: Location,
    mounted: Arc<Mount>,
}

impl MountTable {
    // The caller holds the table lock. Only bounded identity checks and an
    // ownership transfer occur here; prepared storage stays caller-owned on error.
    fn attach(&mut self, prepared: &mut Option<Arc<Mountpoint>>) -> Result<bool, Errno> {
        let candidate = prepared.as_ref().unwrap();
        let target = &candidate.covered;
        // Another mount may have covered this target or one of its parents
        // while lookup was unlocked. Resolve again instead of mounting a hidden node.
        if self
            .points
            .iter()
            .flatten()
            .any(|point| point.covered.is_ancestor_of(target))
        {
            return Ok(false);
        }
        for point in self.points.iter().flatten() {
            if Arc::ptr_eq(&point.mounted.backend, &candidate.mounted.backend) {
                return Err(errno::EINVAL);
            }
            if (Arc::ptr_eq(&point.mounted, &target.0.mount) && target.node() == point.mounted.root)
                || target.is_ancestor_of(&point.covered)
            {
                return Err(errno::EBUSY);
            }
        }
        if self.len == MAX_MOUNTS {
            return Err(errno::ENOMEM);
        }
        self.points[self.len] = prepared.take();
        self.len += 1;
        Ok(true)
    }
}

static NAMESPACE: OncePublication<Namespace> = OncePublication::new();

impl Namespace {
    /// Build a namespace around one root instance; allocates in task/bootstrap context.
    /// # Arguments
    /// * `backend` - Owned root filesystem instance.
    /// # Returns
    /// Namespace with root pinned and no additional mounts.
    /// # Errors
    /// `ENOTDIR` if the backend root is not a directory.
    pub(crate) fn new(backend: Arc<dyn Backend>) -> Result<Self, Errno> {
        let node = backend.root();
        if node.kind != Kind::Directory {
            return Err(errno::ENOTDIR);
        }
        let mount = Arc::new(Mount {
            backend,
            root: node,
        });
        let root = Location(Arc::new(LocationInner {
            mount,
            node,
            absolute: b"/".to_vec(),
            parent: None,
        }));
        Ok(Self {
            root,
            mounts: SpinLock::new(MountTable {
                points: [const { None }; MAX_MOUNTS],
                len: 0,
            }),
        })
    }

    /// Clone the pinned namespace root.
    /// # Returns
    /// Reference-counted location, without allocation or blocking.
    pub(crate) fn root(&self) -> Location {
        self.root.clone()
    }

    /// Mount a distinct instance synchronously in task/bootstrap context.
    /// Nested mounts are allowed; ancestor mounts cannot hide earlier mounts.
    /// Mountpoints use mount/node identity so backend aliases cross the same mount.
    /// # Arguments
    /// * `path` - Absolute existing mountpoint resolved synchronously.
    /// * `backend` - Distinct filesystem instance with directory root.
    /// # Returns
    /// Success after pinning the instance; allocates outside shared locks.
    /// # Errors
    /// Path/backend errors, `EINVAL` for relative/reused instances, `EBUSY`
    /// for conflicting/root/shadowing mounts, `ENOMEM` for bounded capacity.
    #[cfg(any(test, feature = "qemu-test-vfs"))]
    pub(crate) fn mount(&mut self, path: &[u8], backend: Arc<dyn Backend>) -> Result<(), Errno> {
        if !path.starts_with(b"/") {
            return Err(errno::EINVAL);
        }
        loop {
            let mut resolver = Resolver::new(self.root(), path, false)?;
            let location = match resolver.poll(self, None) {
                Poll::Ready(result) => result?,
                Poll::Pending => return Err(errno::ENOTSUP),
            };
            if self.attach(location, backend.clone())? {
                return Ok(());
            }
        }
    }

    /// Count published additional mounts for host assertions.
    /// # Returns
    /// Bounded count, without allocation, callbacks or blocking.
    #[cfg(test)]
    pub(super) fn mount_count(&self) -> usize {
        self.mounts.lock().len
    }

    fn snapshot(&self) -> MountTable {
        // Only Arc increments occur under this lock. The snapshot's references
        // (and any final destructors) are released after the guard is gone.
        self.mounts.lock().clone()
    }

    /// Validate and atomically publish an already constructed filesystem.
    /// Conflict checks and insertion share one table lock. Filesystem lookup,
    /// callbacks, allocation and destruction run outside it.
    /// # Arguments
    /// * `location` - Existing target directory, pinned by that resolution.
    /// * `backend` - New, exclusively reserved filesystem instance.
    /// # Returns
    /// `true` after publication; `false` when a mount covered the target's
    /// pinned ancestry, requiring fresh root-based resolution. Unrelated mounts
    /// do not require a retry. No partial registration is visible on failure.
    /// # Errors
    /// `EINVAL` for reused/invalid instances, `ENOTDIR` for a file target,
    /// `EBUSY` for root/duplicate/shadowing mounts, `ENOMEM` at capacity.
    pub(super) fn attach(
        &self,
        location: Location,
        backend: Arc<dyn Backend>,
    ) -> Result<bool, Errno> {
        let node = backend.root();
        if node.kind != Kind::Directory {
            return Err(errno::EINVAL);
        }
        if Arc::ptr_eq(&self.root.0.mount.backend, &backend) {
            return Err(errno::EINVAL);
        }
        if location.node().kind != Kind::Directory {
            return Err(errno::ENOTDIR);
        }
        if location.0.parent.is_none() {
            return Err(errno::EBUSY);
        }
        let mut point = Some(Arc::new(Mountpoint {
            covered: location,
            mounted: Arc::new(Mount {
                backend,
                root: node,
            }),
        }));
        let result = self.mounts.lock().attach(&mut point);
        // A rejected candidate is destroyed after the table guard is gone.
        drop(point);
        result
    }

    /// Publish the global namespace permanently; runtime attachments stay bounded.
    /// # Arguments
    /// * `self` - Exclusively constructed namespace.
    /// # Returns
    /// Success; publication does not allocate or permit future unmount.
    /// # Errors
    /// `EBUSY` if already published.
    pub(crate) fn publish(self) -> Result<(), Errno> {
        NAMESPACE.publish(self).map_err(|_| errno::EBUSY)
    }

    fn child(
        &self,
        snapshot: &MountTable,
        parent: &Location,
        name: &[u8],
        node: Node,
    ) -> Result<Location, Errno> {
        let separator = usize::from(parent.path() != b"/");
        let length = parent.path().len() + separator + name.len();
        if name.len() > NAME_MAX || length > GENRT_PATH_MAX {
            return Err(errno::ENAMETOOLONG);
        }
        let mut absolute = Vec::new();
        absolute
            .try_reserve_exact(length)
            .map_err(|_| errno::ENOMEM)?;
        absolute.extend_from_slice(parent.path());
        if separator != 0 {
            absolute.push(b'/');
        }
        absolute.extend_from_slice(name);
        let mount = snapshot
            .points
            .iter()
            .flatten()
            .find(|point| {
                Arc::ptr_eq(&point.covered.0.mount, &parent.0.mount) && point.covered.node() == node
            })
            .map(|point| point.mounted.clone())
            .unwrap_or_else(|| parent.0.mount.clone());
        let node = if Arc::ptr_eq(&mount, &parent.0.mount) {
            node
        } else {
            mount.root
        };
        Ok(Location(Arc::new(LocationInner {
            mount,
            node,
            absolute,
            parent: Some(parent.clone()),
        })))
    }
}

/// Borrow the permanently published global namespace.
/// # Returns
/// Namespace reference, without allocation or blocking.
/// # Errors
/// `EINVAL` before bootstrap publication.
pub(crate) fn namespace() -> Result<&'static Namespace, Errno> {
    NAMESPACE.get().ok_or(errno::EINVAL)
}

/// Owned component traversal retained over backend waits.
pub(crate) struct Resolver {
    topology: Option<MountTable>,
    input: Vec<u8>,
    cursor: usize,
    current: Location,
    request: Option<Request>,
    component: Vec<u8>,
    create: bool,
    require_directory: bool,
}

impl Resolver {
    /// Start bounded traversal; allocates only in task work.
    /// # Arguments
    /// * `cwd` - Pinned directory for relative traversal.
    /// * `input` - Original path; components are looked up before collapsing `..`.
    /// * `create` - Permit creating a missing final regular component.
    /// # Returns
    /// Owned restart state; no backend work yet.
    /// # Errors
    /// `ENOENT` for empty input, `ENAMETOOLONG` beyond path bounds,
    /// `EINVAL` for NUL, `ENOTDIR` for invalid cwd.
    pub(crate) fn new(cwd: Location, input: &[u8], create: bool) -> Result<Self, Errno> {
        if input.is_empty() {
            return Err(errno::ENOENT);
        }
        if input.len() > GENRT_PATH_MAX {
            return Err(errno::ENAMETOOLONG);
        }
        if input.contains(&0) {
            return Err(errno::EINVAL);
        }
        if cwd.node().kind != Kind::Directory {
            return Err(errno::ENOTDIR);
        }
        let mut current = cwd;
        if input.starts_with(b"/") {
            while let Some(parent) = current.0.parent.clone() {
                current = parent;
            }
        }
        let last = input.split(|b| *b == b'/').rfind(|c| !c.is_empty());
        let require_directory = input.ends_with(b"/") || matches!(last, Some(b"." | b".."));
        Ok(Self {
            topology: None,
            input: input.to_vec(),
            cursor: 0,
            current,
            request: None,
            component: Vec::new(),
            create,
            require_directory,
        })
    }

    /// Advance traversal or resume its exact backend submission, without sleeping.
    /// # Arguments
    /// * `namespace` - Published namespace; first poll pins a coherent bounded
    ///   topology snapshot retained across backend waits.
    /// * `wake` - Prepared token passed to backend submissions.
    /// # Returns
    /// Pinned target, or `Pending` after exact backend wait publication.
    /// # Errors
    /// Lookup/create errors and bounded path/directory validation errors.
    pub(crate) fn poll(
        &mut self,
        namespace: &Namespace,
        wake: Option<WaitToken>,
    ) -> Poll<Result<Location, Errno>> {
        if self.topology.is_none() {
            self.topology = Some(namespace.snapshot());
        }
        loop {
            if let Some(request) = self.request.as_mut() {
                let result = match self.current.poll(request, wake) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(r) => r,
                };
                if matches!(result, Err(errno::ENOENT))
                    && self.create
                    && !self.require_directory
                    && self.input[self.cursor..].iter().all(|b| *b == b'/')
                    && matches!(request.operation, Operation::Lookup(_))
                {
                    self.request = Some(Request::new(Operation::Create(self.component.clone())));
                    continue;
                }
                let node = match result {
                    Ok(Reply::Node(node)) => node,
                    Ok(_) => return Poll::Ready(Err(errno::EIO)),
                    Err(e) => return Poll::Ready(Err(e)),
                };
                self.request = None;
                self.current = match namespace.child(
                    self.topology.as_ref().unwrap(),
                    &self.current,
                    &self.component,
                    node,
                ) {
                    Ok(c) => c,
                    Err(e) => return Poll::Ready(Err(e)),
                };
            }
            while self.input.get(self.cursor) == Some(&b'/') {
                self.cursor += 1;
            }
            if self.cursor == self.input.len() {
                return Poll::Ready(
                    if self.require_directory && self.current.node().kind != Kind::Directory {
                        Err(errno::ENOTDIR)
                    } else {
                        Ok(self.current.clone())
                    },
                );
            }
            if self.current.node().kind != Kind::Directory {
                return Poll::Ready(Err(errno::ENOTDIR));
            }
            let start = self.cursor;
            while self.cursor < self.input.len() && self.input[self.cursor] != b'/' {
                self.cursor += 1;
            }
            let component = &self.input[start..self.cursor];
            if component.len() > NAME_MAX {
                return Poll::Ready(Err(errno::ENAMETOOLONG));
            }
            if component == b"." {
                continue;
            }
            if component == b".." {
                if let Some(parent) = self.current.0.parent.clone() {
                    self.current = parent;
                }
                continue;
            }
            self.component = component.to_vec();
            let separator = usize::from(self.current.path() != b"/");
            if self.current.path().len() + separator + component.len() > GENRT_PATH_MAX {
                return Poll::Ready(Err(errno::ENAMETOOLONG));
            }
            self.request = Some(Request::new(Operation::Lookup(self.component.clone())));
        }
    }
}
