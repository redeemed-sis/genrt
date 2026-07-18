use crate::{
    loader::elf::{self, UserElfImage},
    memory::vm::{self, OwnedUserAddressSpace},
};

use super::{files::ProcessFileState, id::ProcessId};

/// Process-owned address-space and loaded-image resources.
pub(super) struct ProcessImageResources {
    pub(super) address_space: Option<OwnedUserAddressSpace>,
    pub(super) user_image: Option<UserElfImage>,
}

impl ProcessImageResources {
    pub(super) const fn empty() -> Self {
        Self {
            address_space: None,
            user_image: None,
        }
    }
}

/// All resources whose lifetime belongs to one process.
pub(super) struct ProcessResources {
    pub(super) image: ProcessImageResources,
    pub(super) files: ProcessFileState,
}

impl ProcessResources {
    pub(super) const fn free() -> Self {
        Self {
            image: ProcessImageResources::empty(),
            files: ProcessFileState::free(),
        }
    }

    pub(super) const fn new(files: ProcessFileState) -> Self {
        Self {
            image: ProcessImageResources::empty(),
            files,
        }
    }
}

pub(super) fn cleanup_process_image_resources(pid: ProcessId, resources: ProcessImageResources) {
    if let Some(image) = resources.user_image {
        elf::free_loaded_segments(&image);
    }
    if let Some(address_space) = resources.address_space {
        // SAFETY: resources were atomically removed from the process aggregate
        // and scheduler before cleanup, so no runnable thread references this root.
        if let Err(err) = unsafe { vm::destroy_user_address_space(address_space) } {
            crate::warn!("process: failed to destroy old address space pid={pid}: {err:?}");
        }
    }
}
