use alloc::vec::Vec;

use crate::limits::GENRT_PATH_MAX;

use super::ramfs;

/// Existing ramfs node selected by pathname traversal.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ResolvedNode {
    /// Readonly regular file with its stable mount index.
    File { file_index: usize },
    /// Directory with its stable mount index.
    Directory { dir_index: usize },
}

/// Canonical absolute pathname and node produced by filesystem traversal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPath {
    /// Canonical absolute path with no trailing slash except for root.
    pub absolute: Vec<u8>,
    /// Existing ramfs node reached by traversing the original components.
    pub node: ResolvedNode,
    /// Whether trailing syntax requires the target to be a directory.
    pub requires_directory: bool,
}

/// Errors produced while resolving an existing ramfs pathname.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PathError {
    /// The input pathname is empty.
    Empty,
    /// A traversed component does not exist.
    NotFound,
    /// Traversal attempted to continue through a regular file.
    NotDirectory,
    /// The canonical result would exceed `GENRT_PATH_MAX`.
    NameTooLong,
    /// The supplied cwd directory index is invalid.
    InvalidBase,
    /// Heap allocation failed while constructing the canonical path.
    NoMemory,
}

/// Resolve an existing pathname by traversing immutable ramfs directories.
///
/// Each ordinary component is looked up before the next component is handled,
/// so a later `..` cannot erase a missing component or a regular file used as a
/// directory. Repeated separators and `.` stay in the current directory. `..`
/// moves to the current directory's parent without escaping above `/`.
///
/// # Arguments
///
/// * `cwd_dir` - Stable ramfs directory index used for relative input.
/// * `input` - Absolute or cwd-relative pathname to resolve.
///
/// # Returns
///
/// Returns the canonical absolute pathname, the existing target node, and
/// whether trailing syntax requires a directory target.
///
/// # Errors
///
/// Returns `PathError::Empty` for empty input, `PathError::InvalidBase` for an
/// invalid cwd, `PathError::NotFound` for a missing component,
/// `PathError::NotDirectory` when traversal crosses a regular file,
/// `PathError::NameTooLong` for a canonical result beyond `GENRT_PATH_MAX`, and
/// `PathError::NoMemory` if result allocation fails.
pub fn resolve_existing_path(cwd_dir: usize, input: &[u8]) -> Result<ResolvedPath, PathError> {
    if input.is_empty() {
        return Err(PathError::Empty);
    }

    let root_dir = ramfs::root_dir_index().ok_or(PathError::InvalidBase)?;
    let mut current_dir = if input.starts_with(b"/") {
        root_dir
    } else {
        ramfs::dir_path(cwd_dir).ok_or(PathError::InvalidBase)?;
        cwd_dir
    };
    let cwd = ramfs::dir_path(current_dir).ok_or(PathError::InvalidBase)?;
    if cwd.len() > GENRT_PATH_MAX {
        return Err(PathError::NameTooLong);
    }

    let capacity = if input.starts_with(b"/") {
        input.len().min(GENRT_PATH_MAX)
    } else {
        cwd.len()
            .checked_add(1)
            .and_then(|length| length.checked_add(input.len()))
            .unwrap_or(GENRT_PATH_MAX)
            .min(GENRT_PATH_MAX)
    };
    let mut absolute = Vec::new();
    absolute
        .try_reserve_exact(capacity.max(1))
        .map_err(|_| PathError::NoMemory)?;
    absolute.extend_from_slice(cwd);

    let final_component = input
        .split(|byte| *byte == b'/')
        .rev()
        .find(|component| !component.is_empty());
    let requires_directory = input.ends_with(b"/")
        || matches!(final_component, Some(component) if component == b"." || component == b"..");
    let mut node = ResolvedNode::Directory {
        dir_index: current_dir,
    };

    for component in input.split(|byte| *byte == b'/') {
        if component.is_empty() {
            continue;
        }
        if !matches!(node, ResolvedNode::Directory { .. }) {
            return Err(PathError::NotDirectory);
        }
        if component == b"." {
            continue;
        }
        if component == b".." {
            current_dir = parent_dir(current_dir, root_dir)?;
            replace_with_dir_path(&mut absolute, current_dir)?;
            node = ResolvedNode::Directory {
                dir_index: current_dir,
            };
            continue;
        }

        append_component(&mut absolute, component)?;
        node = if let Some(dir_index) = ramfs::lookup_dir(&absolute) {
            current_dir = dir_index;
            ResolvedNode::Directory { dir_index }
        } else if let Some(file_index) = ramfs::lookup_file(&absolute) {
            ResolvedNode::File { file_index }
        } else {
            return Err(PathError::NotFound);
        };
    }

    if requires_directory && !matches!(node, ResolvedNode::Directory { .. }) {
        return Err(PathError::NotDirectory);
    }

    Ok(ResolvedPath {
        absolute,
        node,
        requires_directory,
    })
}

fn parent_dir(current_dir: usize, root_dir: usize) -> Result<usize, PathError> {
    if current_dir == root_dir {
        return Ok(root_dir);
    }
    let current_path = ramfs::dir_path(current_dir).ok_or(PathError::InvalidBase)?;
    let separator = current_path
        .iter()
        .rposition(|byte| *byte == b'/')
        .ok_or(PathError::InvalidBase)?;
    let parent_path = if separator == 0 {
        b"/".as_slice()
    } else {
        &current_path[..separator]
    };
    ramfs::lookup_dir(parent_path).ok_or(PathError::InvalidBase)
}

fn replace_with_dir_path(path: &mut Vec<u8>, dir_index: usize) -> Result<(), PathError> {
    let directory = ramfs::dir_path(dir_index).ok_or(PathError::InvalidBase)?;
    path.clear();
    path.try_reserve(directory.len())
        .map_err(|_| PathError::NoMemory)?;
    path.extend_from_slice(directory);
    Ok(())
}

fn append_component(path: &mut Vec<u8>, component: &[u8]) -> Result<(), PathError> {
    let separator = usize::from(path.as_slice() != b"/");
    let next_len = path
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(component.len()))
        .ok_or(PathError::NameTooLong)?;
    if next_len > GENRT_PATH_MAX {
        return Err(PathError::NameTooLong);
    }
    path.try_reserve(next_len - path.len())
        .map_err(|_| PathError::NoMemory)?;
    if separator != 0 {
        path.push(b'/');
    }
    path.extend_from_slice(component);
    Ok(())
}
