//! Bounded session and exact-wait lifecycle; no locks or runtime owners.

use super::{
    counter,
    format::{
        Config, EARLY, FULL, GENERIC, Header, IDLE, MAGIC, PHASE_COUNT, Phase, Record, bytes,
    },
};
use crate::{
    cpu::CpuId,
    errno::{self, Errno},
    sched::{ThreadId, WaitToken},
    time,
};
use alloc::vec::Vec;

/// Current retained record and its exact wait binding; at most one per owner.
pub(super) struct Cycle {
    pub(super) index: usize,
    pub(super) token: Option<WaitToken>,
    pub(super) expired: bool,
}
/// Preallocated trace storage and lifecycle, accessed under runtime leaf ownership.
pub(super) struct Session {
    pub(super) owner: ThreadId,
    cpu: CpuId,
    pub(super) header: Header,
    pub(super) records: Vec<Record>,
    pub(super) active: bool,
    pub(super) cycle: Option<Cycle>,
    completed: u64,
}

impl Session {
    /// Allocate and initialize all slots before publication, outside IRQ exclusion.
    /// # Arguments
    /// * `owner` - Exact thread that may contribute or retrieve records.
    /// * `cpu` - Immutable logical CPU for the owner and its waits.
    /// * `config` - Requested mode, capacity, and focused phase pair.
    /// * `reserve` - Fallible reservation in the caller's task-context heap window.
    /// # Returns
    /// An active, unpublished session with fully initialized fixed storage.
    /// # Errors
    /// Configuration errors from Config::bytes or the reservation's allocation error.
    pub(super) fn allocate(
        owner: ThreadId,
        cpu: CpuId,
        config: Config,
        reserve: impl FnOnce(&mut Vec<Record>, usize) -> Result<(), Errno>,
    ) -> Result<Self, Errno> {
        config.bytes()?;
        let mut records = Vec::new();
        reserve(&mut records, config.max_records as usize)?;
        // Initialize every slot before publication; recording never pushes/grows.
        records.resize(config.max_records as usize, Record::default());
        Ok(Self {
            owner,
            cpu,
            active: true,
            cycle: None,
            completed: 0,
            records,
            header: Header {
                magic: MAGIC,
                version: 1,
                source_version: 1,
                header_bytes: 104,
                record_bytes: 160,
                cpu: cpu.index() as u32,
                owner_index: owner.index() as u32,
                owner_generation: owner.generation(),
                frequency: time::counter_freq_hz(),
                mode: config.mode,
                start_phase: config.start_phase,
                end_phase: config.end_phase,
                capacity: config.max_records,
                ..Header::default()
            },
        })
    }

    /// Check mode selection without reading the counter or allocating.
    /// # Arguments
    /// * `phase` - Probe identity to check against full/focused settings.
    /// # Returns
    /// True if this session selects the phase, independent of active state.
    pub(super) fn selected(&self, phase: Phase) -> bool {
        self.header.mode == FULL
            || phase as u32 == self.header.start_phase
            || phase as u32 == self.header.end_phase
    }

    /// Write one selected phase once in preallocated storage; never allocate.
    /// # Arguments
    /// * `index` - Slot reserved by begin, or slot zero during tests.
    /// * `phase` - Counter field and presence bit to write.
    /// * `supplied` - Earlier observation, or None to read the counter now.
    /// # Returns
    /// Returns unchanged for inactive, unselected, or duplicate probes.
    /// Otherwise records the selected timestamp and presence bit.
    /// # Panics
    /// Panics if an active selected probe's index is outside allocated storage.
    pub(super) fn stamp(&mut self, index: usize, phase: Phase, supplied: Option<u64>) {
        let bit = 1u64 << phase as u32;
        if !self.active || !self.selected(phase) || self.records[index].phase_present & bit != 0 {
            return;
        }
        let value = supplied.unwrap_or_else(counter);
        let record = &mut self.records[index];
        record.counters[phase as usize] = value;
        record.phase_present |= bit;
    }

    /// Begin the owner's next eligible cycle, preserving earlier records on loss.
    /// # Arguments
    /// * `owner` - Exact caller; foreign and inactive calls are ignored.
    /// * `deadline` - Validated absolute counter deadline.
    /// * `early` - True when admission determined blocking is unnecessary.
    /// * `entry` - Staged assembly SVC observation.
    /// # Returns
    /// Returns after bounded initialization or exhaustion accounting.
    pub(super) fn begin(&mut self, owner: ThreadId, deadline: u64, early: bool, entry: u64) {
        if !self.active || owner != self.owner {
            return;
        }
        self.cycle = None;
        if self.header.recorded == self.header.capacity as u64 {
            self.header.lost = self.header.lost.saturating_add(1);
            return;
        }
        let index = self.header.recorded as usize;
        self.header.recorded += 1;
        if early {
            self.header.early += 1;
        }
        self.records[index] = Record {
            ordinal: index as u64,
            thread_generation: owner.generation(),
            requested_deadline: deadline,
            wake_path: if early { EARLY } else { GENERIC },
            ..Record::default()
        };
        self.cycle = Some(Cycle {
            index,
            token: None,
            expired: early,
        });
        self.stamp(index, Phase::SvcEntry, Some(entry));
        self.stamp(index, Phase::DeadlineValidated, None);
    }

    /// Stop collection and finalize incomplete accounting in constant time.
    /// # Returns
    /// Returns with stable records and no current cycle; storage remains owned.
    pub(super) fn freeze(&mut self) {
        self.active = false;
        self.header.incomplete = self.header.recorded - self.completed;
        self.cycle = None;
    }

    fn record_complete(&self, index: usize) -> bool {
        let full = (1u64 << PHASE_COUNT) - 1;
        let selected = if self.header.mode == FULL {
            full
        } else {
            (1 << self.header.start_phase) | (1 << self.header.end_phase)
        };
        let r = &self.records[index];
        let path = if r.wake_path == EARLY {
            (1 << 0) | (1 << 1) | (1 << 13)
        } else {
            full
        };
        let required = selected & path;
        r.phase_present & required == required
            && (r.wake_path == EARLY || r.programmed_deadline != 0)
    }

    /// Bind a prepared wait when thread, CPU, and deadline match the cycle.
    /// # Arguments
    /// * `token` - Generation- and sequence-bearing wait identity.
    /// * `deadline` - Validated deadline for this registration.
    /// # Returns
    /// Returns unchanged for unrelated/inactive waits, or with the exact binding.
    pub(super) fn bind(&mut self, token: WaitToken, deadline: u64) {
        if !self.active || self.owner != token.thread() || self.cpu != token.cpu() {
            return;
        }
        if let Some(c) = self.cycle.as_mut() {
            let r = &mut self.records[c.index];
            if r.requested_deadline == deadline {
                c.token = Some(token);
                r.wait_sequence = token.sequence();
            }
        }
    }

    /// Attach originating IRQ observations once the exact wait expires.
    /// # Arguments
    /// * `token` - Extracted registration to match against the current cycle.
    /// * `deadline` - Time queue's original deadline for the extracted event.
    /// * `entry` - Staged earliest IRQ observation.
    /// * `ack` - Optional staged acknowledgement observation.
    /// # Returns
    /// Record index, or None for inactive, stale, or duplicate expiry.
    pub(super) fn expired(
        &mut self,
        token: WaitToken,
        deadline: u64,
        entry: u64,
        ack: Option<u64>,
    ) -> Option<usize> {
        if !self.active || self.cpu != token.cpu() {
            return None;
        }
        let c = self.cycle.as_mut()?;
        if c.token != Some(token)
            || c.expired
            || self.records[c.index].requested_deadline != deadline
        {
            return None;
        }
        c.expired = true;
        let index = c.index;
        self.stamp(index, Phase::IrqEntry, Some(entry));
        if let Some(ack) = ack {
            self.stamp(index, Phase::Acknowledge, Some(ack));
        }
        self.stamp(index, Phase::ExpiredExtracted, None);
        Some(index)
    }

    /// Stamp post-expiry selection/restoration, retaining originating IRQ data.
    /// # Arguments
    /// * `owner` - Exact selected thread; foreign threads are ignored.
    /// * `phase` - Selected resume boundary.
    /// * `idle` - Mark optimized direct idle wake rather than generic selection.
    /// # Returns
    /// Returns after a selected probe or unchanged if this cycle has not expired.
    pub(super) fn resume(&mut self, owner: ThreadId, phase: Phase, idle: bool) {
        if self.owner != owner {
            return;
        }
        if let Some(c) = &self.cycle {
            if c.expired {
                let index = c.index;
                if idle {
                    self.records[index].wake_path = IDLE;
                }
                self.stamp(index, phase, None);
            }
        }
    }

    /// Consume an expired cycle at its owner's final live EL0 return hook.
    /// # Arguments
    /// * `owner` - Exact returning thread identity.
    /// # Returns
    /// Returns after stamping and accounting, or unchanged for unrelated returns.
    pub(super) fn returned(&mut self, owner: ThreadId) {
        if self.active && self.owner == owner {
            if let Some(c) = &self.cycle {
                if c.expired {
                    let index = c.index;
                    self.stamp(index, Phase::ReturnToUser, None);
                    if self.record_complete(index) {
                        self.completed += 1;
                    }
                    self.cycle = None;
                }
            }
        }
    }

    /// Copy frozen export bytes into bounded kernel staging, without allocation.
    /// # Arguments
    /// * `owner` - Exact thread requesting its results.
    /// * `offset` - Export byte offset; at/beyond EOF copies nothing.
    /// * `output` - Caller-bounded staging, never a userspace reference.
    /// # Returns
    /// Copied byte count, bounded by output and export remainder; zero at EOF.
    /// # Errors
    /// EPERM for foreign ownership or EBUSY while collection is active.
    pub(super) fn read(
        &self,
        owner: ThreadId,
        offset: usize,
        output: &mut [u8],
    ) -> Result<usize, Errno> {
        if owner != self.owner {
            return Err(errno::EPERM);
        }
        if self.active {
            return Err(errno::EBUSY);
        }
        let header = bytes(&self.header);
        let len = header.len() + self.header.recorded as usize * 160;
        let count = output.len().min(len.saturating_sub(offset));
        for (n, byte) in output[..count].iter_mut().enumerate() {
            let at = offset + n;
            *byte = if at < header.len() {
                header[at]
            } else {
                let at = at - header.len();
                bytes(&self.records[at / 160])[at % 160]
            };
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests;
