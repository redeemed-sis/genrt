use super::*;
use std::{
    alloc::{GlobalAlloc, Layout, System, alloc_zeroed, dealloc},
    cell::Cell,
    sync::{Arc, Mutex},
};
use virtio_drivers::transport::{DeviceType, InterruptStatus};
use zerocopy::{FromBytes, Immutable, IntoBytes};

std::thread_local! {
    static TICKS: Cell<u64> = const { Cell::new(0) };
    static STEP: Cell<u64> = const { Cell::new(1) };
    static DMA_ALLOCS: Cell<usize> = const { Cell::new(0) };
    static DMA_FREES: Cell<usize> = const { Cell::new(0) };
    static HEAP_ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;
#[global_allocator]
static HOST_ALLOCATOR: CountingAllocator = CountingAllocator;

// SAFETY: forwards all allocations/deallocations unchanged to System. The
// thread-local counter has const initialization and never allocates itself.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = HEAP_ALLOCS.try_with(|count| count.set(count.get() + 1));
        // SAFETY: caller supplied a valid allocation layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = HEAP_ALLOCS.try_with(|count| count.set(count.get() + 1));
        // SAFETY: caller supplied a valid allocation layout.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: allocation ownership and layout are forwarded unchanged.
        unsafe { System.dealloc(pointer, layout) }
    }
}

fn now() -> u64 {
    TICKS.with(|c| {
        let old = c.get();
        c.set(old.wrapping_add(STEP.with(Cell::get)));
        old
    })
}

struct TestHal;
// SAFETY: the host test owns page-aligned allocations and models physical
// addresses with host pointers. The fake device is synchronous on this thread.
unsafe impl Hal for TestHal {
    fn dma_alloc(pages: usize, _: BufferDirection) -> (u64, NonNull<u8>) {
        let layout = Layout::from_size_align(pages * 4096, 4096).unwrap();
        // SAFETY: nonzero valid layout; test allocations must succeed.
        let va = NonNull::new(unsafe { alloc_zeroed(layout) }).unwrap();
        DMA_ALLOCS.with(|n| n.set(n.get() + 1));
        (va.as_ptr() as u64, va)
    }
    unsafe fn dma_dealloc(_: u64, va: NonNull<u8>, pages: usize) -> i32 {
        DMA_FREES.with(|n| n.set(n.get() + 1));
        // SAFETY: same owner, pointer and layout as allocation.
        unsafe {
            dealloc(
                va.as_ptr(),
                Layout::from_size_align(pages * 4096, 4096).unwrap(),
            );
        }
        0
    }
    unsafe fn mmio_phys_to_virt(pa: u64, _: usize) -> NonNull<u8> {
        NonNull::new(pa as *mut u8).unwrap()
    }
    unsafe fn share(buffer: NonNull<[u8]>, _: BufferDirection) -> u64 {
        buffer.as_ptr() as *mut u8 as u64
    }
    unsafe fn unshare(_: u64, _: NonNull<[u8]>, _: BufferDirection) {}
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Complete,
    Stall,
    WrongToken,
    Short,
    Io,
    Unsupported,
    Reset,
    ConfigChange,
}

struct State {
    disk: [u8; SECTOR_SIZE * 16],
    mode: Mode,
    requests: [u32; 128],
    count: usize,
    negotiated: u64,
    ack_count: usize,
    reset_stalls: bool,
    config_changes: bool,
}

struct TestTransport {
    status: DeviceStatus,
    features: u64,
    descriptors: u64,
    avail: u64,
    used: u64,
    queue_used: bool,
    index: u16,
    state: Arc<Mutex<State>>,
}

impl TestTransport {
    fn new(features: u64) -> (Self, Arc<Mutex<State>>) {
        TICKS.with(|c| c.set(0));
        STEP.with(|c| c.set(1));
        let state = Arc::new(Mutex::new(State {
            disk: [0; SECTOR_SIZE * 16],
            mode: Mode::Complete,
            requests: [0; 128],
            count: 0,
            negotiated: 0,
            ack_count: 0,
            reset_stalls: false,
            config_changes: false,
        }));
        (
            Self {
                status: DeviceStatus::empty(),
                features,
                descriptors: 0,
                avail: 0,
                used: 0,
                queue_used: false,
                index: 0,
                state: state.clone(),
            },
            state,
        )
    }
}

impl Transport for TestTransport {
    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }
    fn read_device_features(&mut self) -> u64 {
        self.features
    }
    fn write_driver_features(&mut self, bits: u64) {
        self.state.lock().unwrap().negotiated = bits;
    }
    fn max_queue_size(&mut self, _: u16) -> u32 {
        8
    }
    fn get_status(&self) -> DeviceStatus {
        if self.state.lock().unwrap().reset_stalls {
            DeviceStatus::DRIVER_OK
        } else {
            self.status
        }
    }
    fn set_status(&mut self, status: DeviceStatus) {
        self.status = status;
    }
    fn set_guest_page_size(&mut self, _: u32) {}
    fn requires_legacy_layout(&self) -> bool {
        false
    }
    fn queue_set(&mut self, queue: u16, size: u32, descriptors: u64, driver: u64, device: u64) {
        assert_eq!((queue, size), (0, 8));
        self.descriptors = descriptors;
        self.avail = driver;
        self.used = device;
        self.queue_used = true;
    }
    fn queue_unset(&mut self, _: u16) {
        self.queue_used = false;
    }
    fn queue_used(&mut self, _: u16) -> bool {
        self.queue_used
    }
    fn ack_interrupt(&mut self) -> InterruptStatus {
        let mut state = self.state.lock().unwrap();
        state.ack_count += 1;
        if state.mode == Mode::ConfigChange {
            InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT
        } else {
            InterruptStatus::empty()
        }
    }
    fn read_config_generation(&self) -> u32 {
        if self.state.lock().unwrap().config_changes {
            now() as u32
        } else {
            0
        }
    }
    fn read_config_space<T: FromBytes + IntoBytes>(
        &self,
        offset: usize,
    ) -> virtio_drivers::Result<T> {
        let bytes = 16u64.to_le_bytes();
        T::read_from_bytes(&bytes[offset..offset + mem::size_of::<T>()])
            .map_err(|_| virtio_drivers::Error::ConfigSpaceTooSmall)
    }
    fn write_config_space<T: IntoBytes + Immutable>(
        &mut self,
        _: usize,
        _: T,
    ) -> virtio_drivers::Result<()> {
        unreachable!()
    }
    fn notify(&mut self, queue: u16) {
        assert_eq!(queue, 0);
        let mut state = self.state.lock().unwrap();
        // SAFETY: fake device accesses only live DMA allocations published by
        // VirtQueue. Descriptor/ring layouts here follow the split-ring ABI.
        unsafe {
            assert_eq!(
                (self.avail as *const u16).read_volatile(),
                1,
                "device IRQ notifications must be suppressed"
            );
            let token =
                ((self.avail + 4 + (self.index as u64 % 8) * 2) as *const u16).read_volatile();
            let descriptor = self.descriptors + token as u64 * 16;
            let header = (descriptor as *const u64).read_volatile();
            assert_eq!(((descriptor + 8) as *const u32).read_volatile(), 16);
            let operation = (header as *const u32).read_volatile();
            let sector = ((header + 8) as *const u64).read_volatile() as usize;
            assert_eq!(((header + 4) as *const u32).read_volatile(), 0);
            let index = state.count;
            state.requests[index] = operation;
            state.count += 1;
            if state.mode == Mode::Stall || state.mode == Mode::ConfigChange {
                return;
            }
            if state.mode == Mode::Reset {
                self.status |= DeviceStatus::DEVICE_NEEDS_RESET;
                return;
            }
            let next = ((descriptor + 14) as *const u16).read_volatile();
            let descriptor = self.descriptors + next as u64 * 16;
            let mut status_descriptor = descriptor;
            let used = if operation != 4 {
                let data = (descriptor as *const u64).read_volatile();
                assert_eq!(
                    ((descriptor + 8) as *const u32).read_volatile(),
                    SECTOR_SIZE as u32
                );
                let flags = ((descriptor + 12) as *const u16).read_volatile();
                assert_eq!(flags & 2 != 0, operation == 0);
                if operation == 0 {
                    core::ptr::copy_nonoverlapping(
                        state.disk.as_ptr().add(sector * SECTOR_SIZE),
                        data as *mut u8,
                        SECTOR_SIZE,
                    );
                } else {
                    assert_eq!(operation, 1);
                    core::ptr::copy_nonoverlapping(
                        data as *const u8,
                        state.disk.as_mut_ptr().add(sector * SECTOR_SIZE),
                        SECTOR_SIZE,
                    );
                }
                status_descriptor = self.descriptors
                    + ((descriptor + 14) as *const u16).read_volatile() as u64 * 16;
                if operation == 0 {
                    SECTOR_SIZE as u32 + 1
                } else {
                    1
                }
            } else {
                assert_eq!(sector, 0);
                1
            };
            assert_eq!(((status_descriptor + 8) as *const u32).read_volatile(), 1);
            assert_eq!(((status_descriptor + 12) as *const u16).read_volatile(), 2);
            let status = (status_descriptor as *const u64).read_volatile() as *mut u8;
            status.write_volatile(match state.mode {
                Mode::Io => 1,
                Mode::Unsupported => 2,
                _ => 0,
            });
            let used_entry = self.used + 4 + (self.index as u64 % 8) * 8;
            (used_entry as *mut u32)
                .write_volatile(token as u32 + u32::from(state.mode == Mode::WrongToken));
            ((used_entry + 4) as *mut u32).write_volatile(if state.mode == Mode::Short {
                0
            } else {
                used
            });
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            self.index = self.index.wrapping_add(1);
            ((self.used + 2) as *mut u16).write_volatile(self.index);
        }
    }
}

fn make_device(features: u64) -> (VirtioBlock<TestHal, TestTransport>, Arc<Mutex<State>>) {
    let (transport, state) = TestTransport::new(features);
    (VirtioBlock::new(transport, now, 1000).unwrap(), state)
}

#[test]
fn read_write_flush_wrap_ring_and_allocate_only_at_init() {
    let (device, state) = make_device(u64::MAX);
    assert_eq!(device.capacity_sectors(), 16);
    assert!(device.is_read_only());
    let mut device = {
        drop(device);
        make_device(VERSION_1 | FLUSH).0
    };
    let allocations = DMA_ALLOCS.with(Cell::get);
    let heap_allocations = HEAP_ALLOCS.with(Cell::get);
    let mut buffer = [0; 1024];
    for byte in 0..20 {
        buffer.fill(byte);
        device.write_sectors(3, &buffer).unwrap();
        buffer.fill(0);
        device.read_sectors(3, &mut buffer).unwrap();
        assert_eq!(buffer, [byte; 1024]);
        device.sync().unwrap();
    }
    assert_eq!(DMA_ALLOCS.with(Cell::get), allocations);
    assert_eq!(HEAP_ALLOCS.with(Cell::get), heap_allocations);
    assert_eq!(state.lock().unwrap().negotiated, VERSION_1 | FLUSH | RO);
}

#[test]
fn readonly_and_missing_flush_are_explicit_and_do_not_submit() {
    let (mut device, state) = make_device(VERSION_1 | RO);
    assert!(device.is_read_only());
    assert_eq!(
        device.write_sectors(0, &[0; 512]),
        Err(BlockError::ReadOnly)
    );
    assert_eq!(device.sync(), Err(BlockError::Unsupported));
    device.write_sectors(16, &[]).unwrap();
    assert_eq!(state.lock().unwrap().count, 0);
    device.read_sectors(0, &mut [0; 512]).unwrap();
}

#[test]
fn rejects_bounds_and_alignment_before_any_submission() {
    let (mut device, state) = make_device(VERSION_1 | FLUSH);
    assert_eq!(
        device.read_sectors(u64::MAX, &mut [0; 512]),
        Err(BlockError::OutOfBounds)
    );
    assert_eq!(
        device.write_sectors(16, &[0; 512]),
        Err(BlockError::OutOfBounds)
    );
    assert_eq!(
        device.write_sectors(0, &[0; 513]),
        Err(BlockError::InvalidBuffer)
    );
    assert_eq!(
        device.write_sectors(0, &[0; 512 * 129]),
        Err(BlockError::TransferTooLarge)
    );
    assert_eq!(state.lock().unwrap().count, 0);
}

#[test]
fn timeout_poison_retains_every_dma_page_and_caller_is_not_shared() {
    let frees = DMA_FREES.with(Cell::get);
    let (mut device, state) = make_device(VERSION_1 | FLUSH);
    state.lock().unwrap().mode = Mode::Stall;
    let mut buffer = [42; 512];
    assert_eq!(
        device.read_sectors(0, &mut buffer),
        Err(BlockError::Timeout)
    );
    assert_eq!(buffer, [42; 512]);
    assert_eq!(
        device.read_sectors(0, &mut buffer),
        Err(BlockError::Poisoned)
    );
    assert_eq!(device.write_sectors(0, &buffer), Err(BlockError::Poisoned));
    assert_eq!(device.sync(), Err(BlockError::Poisoned));
    assert_eq!(state.lock().unwrap().count, 1);
    drop(device);
    assert_eq!(DMA_FREES.with(Cell::get), frees);
}

#[test]
fn malformed_completion_reset_and_config_change_fail_closed() {
    for mode in [Mode::WrongToken, Mode::Reset, Mode::ConfigChange] {
        let frees = DMA_FREES.with(Cell::get);
        let (mut device, state) = make_device(VERSION_1 | FLUSH);
        state.lock().unwrap().mode = mode;
        assert_eq!(device.sync(), Err(BlockError::Io));
        assert_eq!(device.sync(), Err(BlockError::Poisoned));
        drop(device);
        assert_eq!(DMA_FREES.with(Cell::get), frees);
    }
}

#[test]
fn completed_status_errors_and_short_used_length_are_reported() {
    for (mode, expected) in [
        (Mode::Io, BlockError::Io),
        (Mode::Unsupported, BlockError::Unsupported),
        (Mode::Short, BlockError::Io),
    ] {
        let (mut device, state) = make_device(VERSION_1 | FLUSH);
        state.lock().unwrap().mode = mode;
        assert_eq!(device.read_sectors(0, &mut [0; 512]), Err(expected));
        state.lock().unwrap().mode = Mode::Complete;
        device.sync().unwrap();
    }
}

#[test]
fn whole_transfer_budget_bounds_many_fast_completed_requests() {
    let (mut device, state) = make_device(VERSION_1 | FLUSH);
    STEP.with(|c| c.set(150));
    assert_eq!(
        device.write_sectors(0, &[1; 512 * 16]),
        Err(BlockError::Timeout)
    );
    assert!(state.lock().unwrap().count < 16);
}

#[test]
fn init_negotiation_reset_and_config_waits_are_bounded() {
    let (transport, _) = TestTransport::new(FLUSH);
    assert!(matches!(
        VirtioBlock::<TestHal, _>::new(transport, now, 1000),
        Err(BlockError::Unsupported)
    ));
    for reset in [true, false] {
        let (transport, state) = TestTransport::new(VERSION_1 | FLUSH);
        state.lock().unwrap().reset_stalls = reset;
        state.lock().unwrap().config_changes = !reset;
        assert!(matches!(
            VirtioBlock::<TestHal, _>::new(transport, now, 1000),
            Err(BlockError::Timeout)
        ));
    }
}

#[test]
fn reset_failure_at_drop_leaks_and_normal_drop_frees() {
    let frees = DMA_FREES.with(Cell::get);
    let (device, state) = make_device(VERSION_1 | FLUSH);
    state.lock().unwrap().reset_stalls = true;
    drop(device);
    assert_eq!(DMA_FREES.with(Cell::get), frees);
    let (device, _) = make_device(VERSION_1 | FLUSH);
    drop(device);
    assert_eq!(DMA_FREES.with(Cell::get), frees + 3);
}

#[test]
fn elapsed_supports_counter_wraparound() {
    assert!(!elapsed(3, u64::MAX - 5, 10));
    assert!(elapsed(4, u64::MAX - 5, 10));
}
