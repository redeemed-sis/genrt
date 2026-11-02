//! Virtio block protocol and one-outstanding-request split queue.
//!
//! The queue crate owns descriptor ordering. Only VERSION_1, FLUSH and RO are
//! negotiated: no indirect descriptors, event_idx, packed queues or per-I/O
//! allocation. QEMU virt provides coherent DMA to normal write-back RAM.

use core::{marker::PhantomData, mem, ops::Range, ptr::NonNull};

use kernel::block::{BlockDevice, BlockError, SECTOR_SIZE, validate_transfer};
use virtio_drivers::{
    BufferDirection, Hal,
    queue::VirtQueue,
    transport::{DeviceStatus, Transport},
};

const VERSION_1: u64 = 1 << 32;
const FLUSH: u64 = 1 << 9;
const RO: u64 = 1 << 5;
const REQUEST_QUEUE: u16 = 0;
const QUEUE_SIZE: usize = 8;

// Virtio-blk request header: little-endian type, reserved word, sector number.
const HEADER_TYPE: Range<usize> = 0..4;
const HEADER_RESERVED: Range<usize> = 4..8;
const HEADER_SECTOR: Range<usize> = 8..16;
const HEADER_SIZE: usize = 16;
const DATA_OFFSET: usize = HEADER_SIZE;
const STATUS_OFFSET: usize = DATA_OFFSET + SECTOR_SIZE;
const STATUS_SIZE: usize = 1;

const STATUS_OK: u8 = 0;
const STATUS_UNSUPPORTED: u8 = 2;
// Driver sentinel, not a device-defined completion status.
const STATUS_UNWRITTEN: u8 = 0xff;

#[repr(u32)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum Operation {
    Read = 0,
    Write = 1,
    Flush = 4,
}

struct Request<H: Hal> {
    pa: u64,
    va: NonNull<u8>,
    _hal: PhantomData<H>,
}

// SAFETY: exclusive ownership moves the DMA allocation; no CPU-local aliases
// exist, and callers never receive a reference to the internal request page.
unsafe impl<H: Hal> Send for Request<H> {}

impl<H: Hal> Request<H> {
    fn new() -> Result<Self, BlockError> {
        let (pa, va) = H::dma_alloc(1, BufferDirection::Both);
        if pa == 0 {
            return Err(BlockError::Io);
        }
        Ok(Self {
            pa,
            va,
            _hal: PhantomData,
        })
    }
}

impl<H: Hal> Drop for Request<H> {
    fn drop(&mut self) {
        // SAFETY: only completed/reset resources reach Drop. An uncertain DMA
        // lifetime causes the entire Resources owner to be forgotten instead.
        unsafe {
            H::dma_dealloc(self.pa, self.va, 1);
        }
    }
}

struct Resources<H: Hal> {
    queue: VirtQueue<H, QUEUE_SIZE>,
    request: Request<H>,
}

/// Exclusive bounded transport owner; all I/O storage is allocated at init.
pub(super) struct VirtioBlock<H: Hal, T: Transport> {
    transport: T,
    resources: Option<Resources<H>>,
    capacity: u64,
    readonly: bool,
    flush: bool,
    poisoned: bool,
    now: fn() -> u64,
    request_ticks: u64,
    transfer_ticks: u64,
}

impl<H: Hal, T: Transport> VirtioBlock<H, T> {
    /// Negotiate a modern block transport and allocate fixed DMA storage.
    /// Ordinary-thread only; no locks or IRQ-state changes. Init/reset and
    /// config consistency waits use the supplied counter deadline.
    /// # Arguments
    /// * `transport` - Exclusive modern block transport with resident MMIO.
    /// * `now` - Monotonic architected-counter reader, including wraparound.
    /// * `frequency` - Nonzero counter frequency in ticks per second.
    /// # Returns
    /// A ready device with one 1-second request and 5-second transfer budget.
    /// # Errors
    /// Returns unsupported for missing VERSION_1/invalid frequency, I/O for
    /// allocation/config/queue failure, or timeout for reset/config deadlines.
    pub(super) fn new(
        mut transport: T,
        now: fn() -> u64,
        frequency: u64,
    ) -> Result<Self, BlockError> {
        let transfer_ticks = frequency
            .checked_mul(5)
            .filter(|ticks| *ticks < i64::MAX as u64)
            .ok_or(BlockError::Unsupported)?;
        if frequency == 0 || transport.requires_legacy_layout() {
            return Err(BlockError::Unsupported);
        }
        transport.set_status(DeviceStatus::empty());
        let start = now();
        while transport.get_status() != DeviceStatus::empty() {
            if elapsed(now(), start, frequency) {
                return Err(BlockError::Timeout);
            }
            core::hint::spin_loop();
        }
        transport.set_status(DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER);
        let features = transport.read_device_features();
        if features & VERSION_1 == 0 {
            return Err(BlockError::Unsupported);
        }
        let negotiated = features & (VERSION_1 | FLUSH | RO);
        transport.write_driver_features(negotiated);
        let ready = DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER | DeviceStatus::FEATURES_OK;
        transport.set_status(ready);
        if transport.get_status() != ready {
            return Err(BlockError::Unsupported);
        }

        let start = now();
        let capacity = loop {
            if elapsed(now(), start, frequency) {
                return Err(BlockError::Timeout);
            }
            let generation = transport.read_config_generation();
            let low: u32 = transport.read_config_space(0).map_err(|_| BlockError::Io)?;
            let high: u32 = transport.read_config_space(4).map_err(|_| BlockError::Io)?;
            if generation == transport.read_config_generation() {
                break (u32::from_le(low) as u64) | ((u32::from_le(high) as u64) << 32);
            }
        };
        if capacity == 0 {
            return Err(BlockError::Io);
        }
        let request = Request::new()?;
        let mut queue = VirtQueue::new(&mut transport, REQUEST_QUEUE, false, false)
            .map_err(|_| BlockError::Io)?;
        queue.set_dev_notify(false);
        let mut device = Self {
            transport,
            resources: Some(Resources { queue, request }),
            capacity,
            readonly: negotiated & RO != 0,
            flush: negotiated & FLUSH != 0,
            poisoned: false,
            now,
            request_ticks: frequency,
            transfer_ticks,
        };
        device.transport.ack_interrupt();
        device.transport.finish_init();
        if device.transport.get_status() != ready | DeviceStatus::DRIVER_OK {
            return Err(BlockError::Io);
        }
        Ok(device)
    }

    fn request(
        &mut self,
        operation: Operation,
        sector: u64,
        write: &[u8],
        transfer_start: u64,
    ) -> Result<(), BlockError> {
        if self.poisoned {
            return Err(BlockError::Poisoned);
        }
        if elapsed((self.now)(), transfer_start, self.transfer_ticks) {
            return Err(BlockError::Timeout);
        }
        let resources = self.resources.as_mut().ok_or(BlockError::Poisoned)?;
        let base = resources.request.va.as_ptr();
        // SAFETY: one exclusive owner, no outstanding request. Header, sector,
        // and status are disjoint parts of the permanently allocated DMA page.
        // Only these buffers enter the queue, never a caller/stack buffer. On
        // any uncertain completion the device is poisoned and Resources is
        // retained forever, so the add/pop DMA lifetime cannot outlive backing.
        let (header, data, status) = unsafe {
            let header = core::slice::from_raw_parts_mut(base, HEADER_SIZE);
            header[HEADER_TYPE].copy_from_slice(&(operation as u32).to_le_bytes());
            header[HEADER_RESERVED].fill(0);
            header[HEADER_SECTOR].copy_from_slice(&sector.to_le_bytes());
            let data = core::slice::from_raw_parts_mut(base.add(DATA_OFFSET), SECTOR_SIZE);
            if operation == Operation::Write {
                data.copy_from_slice(write);
            }
            let status = core::slice::from_raw_parts_mut(base.add(STATUS_OFFSET), STATUS_SIZE);
            status[0] = STATUS_UNWRITTEN;
            (header, data, status)
        };
        let mut inputs: [&[u8]; 2] = [header, &[]];
        let mut outputs: [&mut [u8]; 2] = [&mut [], &mut []];
        let (input_count, output_count) = match operation {
            Operation::Read => {
                outputs = [data, status];
                (1, 2)
            }
            Operation::Write => {
                inputs[1] = data;
                outputs[0] = status;
                (2, 1)
            }
            Operation::Flush => {
                outputs[0] = status;
                (1, 1)
            }
        };
        let inputs = &inputs[..input_count];
        let outputs = &mut outputs[..output_count];
        let request_start = (self.now)();
        // SAFETY: the backing and exclusive DMA lifetime are described above.
        let token = unsafe { resources.queue.add(inputs, outputs) }.map_err(|_| BlockError::Io)?;
        if resources.queue.should_notify() {
            self.transport.notify(REQUEST_QUEUE);
        }
        loop {
            let now = (self.now)();
            if elapsed(now, request_start, self.request_ticks)
                || elapsed(now, transfer_start, self.transfer_ticks)
            {
                self.poisoned = true;
                self.transport.ack_interrupt();
                return Err(BlockError::Timeout);
            }
            let interrupt = self.transport.ack_interrupt();
            if self
                .transport
                .get_status()
                .intersects(DeviceStatus::DEVICE_NEEDS_RESET | DeviceStatus::FAILED)
                || interrupt.contains(
                    virtio_drivers::transport::InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT,
                )
            {
                self.poisoned = true;
                return Err(BlockError::Io);
            }
            if resources.queue.can_pop() {
                break;
            }
            core::hint::spin_loop();
        }
        // SAFETY: exact token and exact buffers from this single submission.
        let used = match unsafe { resources.queue.pop_used(token, inputs, outputs) } {
            Ok(used) => used,
            Err(_) => {
                self.poisoned = true;
                return Err(BlockError::Io);
            }
        };
        self.transport.ack_interrupt();
        let expected = if operation == Operation::Read {
            (SECTOR_SIZE + STATUS_SIZE) as u32
        } else {
            STATUS_SIZE as u32
        };
        // SAFETY: successful pop returned DMA ownership of the status byte.
        let status = unsafe { base.add(STATUS_OFFSET).read_volatile() };
        match status {
            STATUS_OK if used == expected => Ok(()),
            STATUS_UNSUPPORTED => Err(BlockError::Unsupported),
            _ => Err(BlockError::Io),
        }
    }
}

impl<H: Hal + Send, T: Transport + Send> BlockDevice for VirtioBlock<H, T> {
    fn capacity_sectors(&self) -> u64 {
        self.capacity
    }

    fn is_read_only(&self) -> bool {
        self.readonly
    }

    fn read_sectors(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        validate_transfer(self.capacity, sector, buffer.len())?;
        if self.poisoned {
            return Err(BlockError::Poisoned);
        }
        let start = (self.now)();
        for (index, chunk) in buffer.chunks_exact_mut(SECTOR_SIZE).enumerate() {
            self.request(Operation::Read, sector + index as u64, &[], start)?;
            let resources = self.resources.as_ref().ok_or(BlockError::Poisoned)?;
            // SAFETY: successful pop returned exclusive ownership; copying ends
            // before the next request and caller buffers never enter DMA.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    resources.request.va.as_ptr().add(DATA_OFFSET),
                    chunk.as_mut_ptr(),
                    SECTOR_SIZE,
                );
            }
        }
        Ok(())
    }

    fn write_sectors(&mut self, sector: u64, buffer: &[u8]) -> Result<(), BlockError> {
        validate_transfer(self.capacity, sector, buffer.len())?;
        if self.poisoned {
            return Err(BlockError::Poisoned);
        }
        if buffer.is_empty() {
            return Ok(());
        }
        if self.readonly {
            return Err(BlockError::ReadOnly);
        }
        let start = (self.now)();
        for (index, chunk) in buffer.chunks_exact(SECTOR_SIZE).enumerate() {
            self.request(Operation::Write, sector + index as u64, chunk, start)?;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), BlockError> {
        if self.poisoned {
            return Err(BlockError::Poisoned);
        }
        if !self.flush {
            return Err(BlockError::Unsupported);
        }
        self.request(Operation::Flush, 0, &[], (self.now)())
    }
}

impl<H: Hal, T: Transport> Drop for VirtioBlock<H, T> {
    fn drop(&mut self) {
        let Some(resources) = self.resources.take() else {
            return;
        };
        if self.poisoned {
            // A timeout is not cancellation. Even if a later reset appears to
            // succeed, retain all potentially device-owned pages permanently.
            mem::forget(resources);
            return;
        }
        self.transport.set_status(DeviceStatus::empty());
        let start = (self.now)();
        while self.transport.get_status() != DeviceStatus::empty() {
            if elapsed((self.now)(), start, self.request_ticks) {
                mem::forget(resources);
                return;
            }
            core::hint::spin_loop();
        }
        drop(resources);
    }
}

fn elapsed(now: u64, start: u64, ticks: u64) -> bool {
    now.wrapping_sub(start) >= ticks
}

#[cfg(test)]
#[path = "queue/tests.rs"]
mod tests;
