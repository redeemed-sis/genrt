use core::fmt;

pub(super) const MAX_PROCESSES: usize = 4;
const PROCESS_ID_INDEX_BITS: usize = process_id_index_bits(MAX_PROCESSES);
const PROCESS_ID_INDEX_MASK: usize = (1 << PROCESS_ID_INDEX_BITS) - 1;

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct ProcessId {
    index: usize,
    generation: u32,
}

impl ProcessId {
    pub(crate) const fn new(index: usize, generation: u32) -> Self {
        Self { index, generation }
    }

    pub(crate) const fn index(self) -> usize {
        self.index
    }

    pub(crate) const fn generation(self) -> u32 {
        self.generation
    }

    pub(crate) const fn as_raw(self) -> usize {
        ((self.generation as usize) << PROCESS_ID_INDEX_BITS) | self.index
    }

    pub(crate) const fn from_raw(raw: usize) -> Option<Self> {
        if raw == 0 {
            return None;
        }
        let index = raw & PROCESS_ID_INDEX_MASK;
        let raw_generation = raw >> PROCESS_ID_INDEX_BITS;
        if raw_generation > u32::MAX as usize {
            return None;
        }
        let generation = raw_generation as u32;
        if index >= MAX_PROCESSES || generation == 0 {
            return None;
        }
        Some(Self { index, generation })
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.index, self.generation)
    }
}

impl fmt::Debug for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessId")
            .field("index", &self.index)
            .field("generation", &self.generation)
            .finish()
    }
}

const fn process_id_index_bits(slots: usize) -> usize {
    let mut bits = 0usize;
    let mut capacity = 1usize;
    while capacity < slots {
        bits += 1;
        capacity <<= 1;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::{PROCESS_ID_INDEX_BITS, ProcessId};

    #[test]
    fn raw_pid_rejects_discarded_generation_bits() {
        let pid = ProcessId::new(0, 1);
        let discarded_bit = (u32::MAX as usize)
            .checked_add(1)
            .and_then(|generation| generation.checked_shl(PROCESS_ID_INDEX_BITS as u32));

        if let Some(discarded_bit) = discarded_bit {
            assert_eq!(ProcessId::from_raw(pid.as_raw() | discarded_bit), None);
        }
    }
}
