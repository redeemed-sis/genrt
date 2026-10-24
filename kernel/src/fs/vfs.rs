//! Boot-built global namespace with pinned mounts and parent-aware locations.

use super::backend::{Backend, Kind, NAME_MAX, Node, Operation, Reply, Request};
use crate::{
    config::GENRT_PATH_MAX,
    errno::{self, Errno},
    sched::WaitToken,
    sync::OncePublication,
};
use alloc::{sync::Arc, vec::Vec};
use core::task::Poll;

#[allow(dead_code)] // Additional registration is currently kernel/test controlled.
const MAX_MOUNTS: usize = 16;

/// A filesystem instance pinned by the namespace and every open location.
pub(crate) struct Mount {
    pub(super) backend: Arc<dyn Backend>,
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
}

/// One namespace, mutable only while exclusively owned during bootstrap.
pub(crate) struct Namespace {
    root: Location,
    mounts: Vec<(Vec<u8>, Arc<Mount>)>,
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
        let mount = Arc::new(Mount { backend });
        let root = Location(Arc::new(LocationInner {
            mount,
            node,
            absolute: b"/".to_vec(),
            parent: None,
        }));
        Ok(Self {
            root,
            mounts: Vec::new(),
        })
    }

    /// Clone the pinned namespace root.
    /// # Returns
    /// Reference-counted location, without allocation or blocking.
    pub(crate) fn root(&self) -> Location {
        self.root.clone()
    }

    /// Mount a distinct instance on an existing directory during exclusive bootstrap.
    /// Nested mounts are allowed; ancestor mounts cannot hide earlier mounts.
    /// # Arguments
    /// * `path` - Absolute existing mountpoint resolved synchronously.
    /// * `backend` - Distinct filesystem instance with directory root.
    /// # Returns
    /// Success after pinning the instance; allocates outside shared locks.
    /// # Errors
    /// Path/backend errors, `EINVAL` for relative/reused instances, `EBUSY`
    /// for conflicting/root/shadowing mounts, `ENOMEM` for bounded capacity.
    #[allow(dead_code)]
    pub(crate) fn mount(&mut self, path: &[u8], backend: Arc<dyn Backend>) -> Result<(), Errno> {
        if !path.starts_with(b"/") || backend.root().kind != Kind::Directory {
            return Err(errno::EINVAL);
        }
        if Arc::ptr_eq(&self.root.0.mount.backend, &backend)
            || self
                .mounts
                .iter()
                .any(|(_, m)| Arc::ptr_eq(&m.backend, &backend))
        {
            return Err(errno::EINVAL);
        }
        let mut resolver = Resolver::new(self.root(), path, false)?;
        let location = match resolver.poll(self, None) {
            Poll::Ready(result) => result?,
            Poll::Pending => return Err(errno::ENOTSUP),
        };
        if location.node().kind != Kind::Directory {
            return Err(errno::ENOTDIR);
        }
        let path = location.path();
        if path == b"/" {
            return Err(errno::EBUSY);
        }
        if self
            .mounts
            .iter()
            .any(|(p, _)| p == path || (p.starts_with(path) && p.get(path.len()) == Some(&b'/')))
        {
            return Err(errno::EBUSY);
        }
        if self.mounts.len() == MAX_MOUNTS {
            return Err(errno::ENOMEM);
        }
        self.mounts.try_reserve(1).map_err(|_| errno::ENOMEM)?;
        self.mounts
            .push((path.to_vec(), Arc::new(Mount { backend })));
        Ok(())
    }

    /// Publish and freeze the global boot namespace permanently.
    /// # Arguments
    /// * `self` - Exclusively constructed namespace.
    /// # Returns
    /// Success; publication does not allocate or permit future unmount.
    /// # Errors
    /// `EBUSY` if already published.
    pub(crate) fn publish(self) -> Result<(), Errno> {
        NAMESPACE.publish(self).map_err(|_| errno::EBUSY)
    }

    fn child(&self, parent: &Location, name: &[u8], node: Node) -> Result<Location, Errno> {
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
        let mount = self
            .mounts
            .iter()
            .find(|(p, _)| *p == absolute)
            .map(|(_, m)| m.clone())
            .unwrap_or_else(|| parent.0.mount.clone());
        let node = if Arc::ptr_eq(&mount, &parent.0.mount) {
            node
        } else {
            mount.backend.root()
        };
        Ok(Location(Arc::new(LocationInner {
            mount,
            node,
            absolute,
            parent: Some(parent.clone()),
        })))
    }
}

/// Borrow the permanently frozen global namespace.
/// # Returns
/// Namespace reference, without allocation or blocking.
/// # Errors
/// `EINVAL` before bootstrap publication.
pub(crate) fn namespace() -> Result<&'static Namespace, Errno> {
    NAMESPACE.get().ok_or(errno::EINVAL)
}

/// Owned component traversal retained over backend waits.
pub(crate) struct Resolver {
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
    /// * `namespace` - Frozen namespace being traversed.
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
        loop {
            if let Some(request) = self.request.as_mut() {
                let result = match self.current.backend().poll(request, wake) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(r) => r,
                };
                if matches!(result, Err(errno::ENOENT))
                    && self.create
                    && !self.require_directory
                    && self.input[self.cursor..].iter().all(|b| *b == b'/')
                    && matches!(request.operation, Operation::Lookup(_))
                {
                    self.request = Some(Request::new(
                        self.current.node(),
                        Operation::Create(self.component.clone()),
                    ));
                    continue;
                }
                let node = match result {
                    Ok(Reply::Node(node)) => node,
                    Ok(_) => return Poll::Ready(Err(errno::EIO)),
                    Err(e) => return Poll::Ready(Err(e)),
                };
                self.request = None;
                self.current = match namespace.child(&self.current, &self.component, node) {
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
            self.request = Some(Request::new(
                self.current.node(),
                Operation::Lookup(self.component.clone()),
            ));
        }
    }
}
