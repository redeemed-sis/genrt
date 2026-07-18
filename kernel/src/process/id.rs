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
        let generation = (raw >> PROCESS_ID_INDEX_BITS) as u32;
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
