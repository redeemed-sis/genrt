use super::{DtbError, Fdt};

// fdt-raw's node iterator deliberately treats a parse error like end-of-tree,
// and its slice constructors expect header offsets/property names in bounds.
// Validate framing first so neither malformed input nor a valid prefix can be
// mistaken for successful traversal. Binding-specific parsing stays in consumers.
pub(super) fn validate(fdt: &Fdt<'_>, bytes: &[u8]) -> Result<(), DtbError> {
    let header = fdt.header();
    let total = header.totalsize as usize;
    if total < 40 || header.version != 17 || header.last_comp_version > 17 {
        return Err(DtbError::Malformed);
    }
    let region = |offset: u32, size: u32| -> Result<&[u8], DtbError> {
        let start = offset as usize;
        let end = start
            .checked_add(size as usize)
            .ok_or(DtbError::Malformed)?;
        if start < 40 || end > total {
            return Err(DtbError::Malformed);
        }
        bytes.get(start..end).ok_or(DtbError::Malformed)
    };
    if !header.off_dt_struct.is_multiple_of(4) {
        return Err(DtbError::Malformed);
    }
    let structure = region(header.off_dt_struct, header.size_dt_struct)?;
    let strings = region(header.off_dt_strings, header.size_dt_strings)?;
    let mut reservation = header.off_mem_rsvmap as usize;
    if reservation < 40 || !reservation.is_multiple_of(8) {
        return Err(DtbError::Malformed);
    }
    loop {
        let end = reservation.checked_add(16).ok_or(DtbError::Malformed)?;
        if end > total {
            return Err(DtbError::Malformed);
        }
        let entry = bytes.get(reservation..end).ok_or(DtbError::Malformed)?;
        if entry.iter().all(|byte| *byte == 0) {
            break;
        }
        reservation = end;
    }
    let mut cursor = 0usize;
    let mut depth = 0usize;
    let mut root_seen = false;
    let mut children = [false; 16];
    let mut path_lengths = [0usize; 16];
    loop {
        let token = be32(structure, cursor)?;
        cursor += 4;
        match token {
            1 => {
                if depth == 0 && root_seen || depth == 16 {
                    return Err(DtbError::Malformed);
                }
                if depth != 0 {
                    children[depth - 1] = true;
                }
                let name = structure.get(cursor..).ok_or(DtbError::Malformed)?;
                let len = name
                    .iter()
                    .position(|b| *b == 0)
                    .ok_or(DtbError::Malformed)?;
                core::str::from_utf8(&name[..len]).map_err(|_| DtbError::Malformed)?;
                if depth == 0 && len != 0 {
                    return Err(DtbError::Malformed);
                }
                path_lengths[depth] = if depth == 0 {
                    0
                } else {
                    path_lengths[depth - 1] + len + 1
                };
                // fdt-raw represents paths in a fixed 256-byte string.
                if path_lengths[depth] > 256 {
                    return Err(DtbError::Unsupported);
                }
                cursor = cursor.checked_add(len + 1).ok_or(DtbError::Malformed)?;
                cursor = cursor.checked_add(3).ok_or(DtbError::Malformed)? & !3;
                children[depth] = false;
                depth += 1;
                root_seen = true;
            }
            2 => {
                depth = depth.checked_sub(1).ok_or(DtbError::Malformed)?;
            }
            3 => {
                if depth == 0 || children[depth - 1] {
                    return Err(DtbError::Malformed);
                }
                let len = be32(structure, cursor)? as usize;
                let offset = be32(structure, cursor + 4)? as usize;
                let name = strings.get(offset..).ok_or(DtbError::Malformed)?;
                let nul = name
                    .iter()
                    .position(|b| *b == 0)
                    .ok_or(DtbError::Malformed)?;
                let name = core::str::from_utf8(&name[..nul]).map_err(|_| DtbError::Malformed)?;
                if matches!(
                    name,
                    "#address-cells"
                        | "#size-cells"
                        | "interrupt-parent"
                        | "#interrupt-cells"
                        | "phandle"
                ) && len != 4
                {
                    return Err(DtbError::Malformed);
                }
                cursor = cursor
                    .checked_add(8)
                    .and_then(|c| c.checked_add(len))
                    .ok_or(DtbError::Malformed)?;
                cursor = cursor.checked_add(3).ok_or(DtbError::Malformed)? & !3;
            }
            4 => {}
            9 if depth == 0 && root_seen && cursor == structure.len() => return Ok(()),
            _ => return Err(DtbError::Malformed),
        }
        if cursor > structure.len() {
            return Err(DtbError::Malformed);
        }
    }
}

fn be32(bytes: &[u8], offset: usize) -> Result<u32, DtbError> {
    let word = bytes
        .get(offset..offset.checked_add(4).ok_or(DtbError::Malformed)?)
        .ok_or(DtbError::Malformed)?;
    Ok(u32::from_be_bytes(
        word.try_into().map_err(|_| DtbError::Malformed)?,
    ))
}
