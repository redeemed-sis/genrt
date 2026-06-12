use alloc::vec::Vec;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PathError {
    Empty,
    NoMemory,
}

pub fn root_relative(path: &[u8]) -> Result<Vec<u8>, PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }

    let needs_root = path[0] != b'/';
    let len = path.len() + usize::from(needs_root);
    let mut out = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|_| PathError::NoMemory)?;
    if needs_root {
        out.push(b'/');
    }
    out.extend_from_slice(path);
    Ok(out)
}
