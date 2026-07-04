use core::cell::UnsafeCell;

use bootinfo::BootInfo;

mod frame_alloc;
pub mod heap;
mod map;
mod types;
pub mod user;
pub mod vm;

use frame_alloc::{FrameAllocator, FreeListStorage};
use map::{
    add_reserved_range, build_memory_map, collect_boot_ranges, merge_ranges, phys_range_from_u64,
    sort_ranges,
};
pub use types::{
    AddrRange, AddrRegion, FrameRange, PAGE_SIZE, PhysAddr, PhysRange, PhysRegion, RegionKind,
    VirtAddr, VirtRange, VirtRegion,
};

const KERNEL_HEAP_BOOTSTRAP_SIZE: usize = 16 * 1024 * 1024;
const MAX_RAM_RANGES: usize = 16;
const MAX_RESERVED_RANGES: usize = 32;
const MAX_PHYS_REGIONS: usize = 64;
const MAX_USABLE_RANGES: usize = 32;

unsafe extern "C" {
    static __kernel_image_phys_start_value: usize;
    static __kernel_image_phys_end_value: usize;
    static __boot_stack_bottom_value: usize;
    static __boot_stack_top_value: usize;
    fn arch_phys_to_virt(pa: usize) -> usize;
}

pub(crate) type Result<T> = core::result::Result<T, MemoryError>;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum MemoryError {
    NoUsableRam,
    NoBootstrapHeapRange,
    TooManyRanges,
    AddressOutOfRange,
    HeapInit(heap::HeapInitError),
    HeapSmokeTest(heap::HeapSmokeError),
}

struct PhysFrameStorage;

impl FreeListStorage<PhysAddr> for PhysFrameStorage {
    #[inline(always)]
    fn free_list_end() -> PhysAddr {
        usize::MAX
    }

    #[inline(always)]
    unsafe fn read_next_free_frame(frame: PhysAddr) -> PhysAddr {
        // SAFETY: free-list metadata is stored in the free physical page. The
        // concrete physical storage policy is the only allocator layer that
        // converts that physical frame address into a high kernel pointer.
        unsafe { phys_to_kernel_ptr::<PhysAddr>(frame).read() }
    }

    #[inline(always)]
    unsafe fn write_next_free_frame(frame: PhysAddr, next: PhysAddr) {
        // SAFETY: same invariant as `read_next_free_frame()`: metadata lives in
        // the free page and is reached through the high direct-map alias.
        unsafe { phys_to_kernel_ptr::<PhysAddr>(frame).write(next) }
    }
}

struct MemoryState {
    initialized: bool,
    phys_regions: [PhysRegion; MAX_PHYS_REGIONS],
    phys_region_count: usize,
    usable_ranges: [FrameRange; MAX_USABLE_RANGES],
    usable_range_count: usize,
    heap_range: Option<FrameRange>,
    allocator: FrameAllocator<PhysAddr, PhysFrameStorage>,
}

impl MemoryState {
    const fn new() -> Self {
        Self {
            initialized: false,
            phys_regions: [PhysRegion {
                range: PhysRange::empty(),
                kind: RegionKind::Reserved,
            }; MAX_PHYS_REGIONS],
            phys_region_count: 0,
            usable_ranges: [FrameRange::empty(); MAX_USABLE_RANGES],
            usable_range_count: 0,
            heap_range: None,
            allocator: FrameAllocator::new(),
        }
    }

    fn reset(&mut self) {
        self.initialized = false;
        self.phys_region_count = 0;
        self.usable_range_count = 0;
        self.heap_range = None;
        self.allocator.reset();
        for region in &mut self.phys_regions {
            *region = PhysRegion {
                range: PhysRange::empty(),
                kind: RegionKind::Reserved,
            };
        }
        for range in &mut self.usable_ranges {
            *range = FrameRange::empty();
        }
    }
}

struct MemoryCell(UnsafeCell<MemoryState>);

// SAFETY: genrt currently mutates memory state only on a single core during bring-up.
unsafe impl Sync for MemoryCell {}

static MEMORY: MemoryCell = MemoryCell(UnsafeCell::new(MemoryState::new()));

pub(crate) fn init(boot: &'static BootInfo) -> Result<()> {
    let mut ram_ranges = [PhysRange::empty(); MAX_RAM_RANGES];
    let mut ram_count = 0usize;
    let mut reserved_ranges = [PhysRange::empty(); MAX_RESERVED_RANGES];
    let mut reserved_count = 0usize;

    collect_boot_ranges(
        boot.memory_map,
        &mut ram_ranges,
        &mut ram_count,
        &mut reserved_ranges,
        &mut reserved_count,
    )?;

    add_reserved_range(
        &mut reserved_ranges,
        &mut reserved_count,
        kernel_image_range(),
        "kernel image",
    )?;
    add_reserved_range(
        &mut reserved_ranges,
        &mut reserved_count,
        boot_stack_range(),
        "boot stack",
    )?;
    if let Some(dtb_range) = dtb_range(boot)? {
        add_reserved_range(&mut reserved_ranges, &mut reserved_count, dtb_range, "dtb")?;
    }
    add_reserved_range(
        &mut reserved_ranges,
        &mut reserved_count,
        vm::initramfs_load_range(),
        "initramfs loader region",
    )?;

    crate::debug!(
        "memory: raw ranges ram={} reserved={}",
        ram_count,
        reserved_count
    );
    sort_ranges(&mut ram_ranges, ram_count);
    sort_ranges(&mut reserved_ranges, reserved_count);
    reserved_count = merge_ranges(&mut reserved_ranges, reserved_count);

    let state = memory_mut();
    state.reset();

    build_memory_map(
        &ram_ranges[..ram_count],
        &reserved_ranges[..reserved_count],
        &mut state.phys_regions,
        &mut state.phys_region_count,
        &mut state.usable_ranges,
        &mut state.usable_range_count,
    )?;

    let usable = &state.usable_ranges[..state.usable_range_count];
    if usable.is_empty() {
        return Err(MemoryError::NoUsableRam);
    }

    for region in &state.phys_regions[..state.phys_region_count] {
        match region.kind {
            RegionKind::Usable => {
                crate::debug!("memory: usable region {:?}", region.range);
            }
            RegionKind::Reserved => {
                crate::debug!("memory: reserved region {:?}", region.range);
            }
            RegionKind::Mmio => {}
        }
    }

    for range in usable {
        crate::debug!(
            "memory: usable pages {:?} frames={}",
            range,
            range.frame_count()
        );
    }

    state.allocator.init_from_ranges(usable, PAGE_SIZE);
    // The bootstrap heap is allocated from the frame allocator before the rest
    // of the kernel starts using heap-backed containers. Ownership is therefore
    // transferred from the frame allocator to the heap subsystem at this point,
    // even though `usable_ranges()` still describes the broader usable RAM set.
    crate::debug!("memory: allocating bootstrap heap from frame allocator");
    let bootstrap_heap_range = state
        .allocator
        .alloc_contiguous(KERNEL_HEAP_BOOTSTRAP_SIZE / PAGE_SIZE, PAGE_SIZE)
        .ok_or(MemoryError::NoBootstrapHeapRange)?;
    crate::debug!(
        "memory: bootstrap heap allocated {:?}",
        bootstrap_heap_range
    );
    state.heap_range = Some(bootstrap_heap_range);

    crate::debug!("memory: initializing linked_list_allocator heap");
    let bootstrap_heap_va = phys_to_kernel_va(bootstrap_heap_range.start);
    heap::init_heap(
        bootstrap_heap_va,
        bootstrap_heap_range.end - bootstrap_heap_range.start,
    )
    .map_err(MemoryError::HeapInit)?;
    crate::debug!("memory: running heap smoke tests");
    heap::run_heap_smoke_tests().map_err(MemoryError::HeapSmokeTest)?;
    crate::debug!("memory: heap smoke tests completed");
    state.initialized = true;

    crate::info!(
        "memory: initialized usable_ranges={} free_frames={} heap_kib={} heap_phys_range={:?} heap_virt_range={:?}",
        state.usable_range_count,
        state.allocator.free_frames(),
        KERNEL_HEAP_BOOTSTRAP_SIZE / 1024,
        bootstrap_heap_range,
        VirtRange {
            start: bootstrap_heap_va,
            end: bootstrap_heap_va + (bootstrap_heap_range.end - bootstrap_heap_range.start),
        }
    );
    run_allocator_self_check(state)?;
    Ok(())
}

pub fn alloc_frame() -> Option<PhysAddr> {
    let state = memory_mut();
    if !state.initialized {
        return None;
    }

    state.allocator.alloc_frame()
}

pub fn alloc_contiguous_frames(frames: usize) -> Option<FrameRange> {
    let state = memory_mut();
    if !state.initialized {
        return None;
    }

    state.allocator.alloc_contiguous(frames, PAGE_SIZE)
}

pub fn free_frame(frame_pa: PhysAddr) {
    let state = memory_mut();
    if !state.initialized {
        panic!("memory: free_frame before init");
    }
    if !is_valid_usable_frame(&state.usable_ranges[..state.usable_range_count], frame_pa) {
        panic!("memory: attempted to free invalid frame {frame_pa}");
    }

    state.allocator.free_frame(frame_pa);
}

pub fn free_contiguous_frames(range: FrameRange) {
    let state = memory_mut();
    if !state.initialized {
        panic!("memory: free_contiguous_frames before init");
    }
    if !is_valid_usable_frame_range(&state.usable_ranges[..state.usable_range_count], range) {
        panic!("memory: attempted to free invalid frame range {:?}", range);
    }

    state.allocator.free_contiguous(range, PAGE_SIZE);
}

pub fn zero_phys_range(range: FrameRange) {
    if range.start >= range.end {
        return;
    }

    let len = range.end - range.start;
    // SAFETY: callers pass physical RAM ranges that are accessible through the
    // kernel direct map. This helper centralizes the PA -> HVA dereference
    // boundary while the generic frame allocator remains address-agnostic.
    unsafe { core::ptr::write_bytes(phys_to_kernel_va(range.start) as *mut u8, 0, len) };
}

pub fn usable_ranges() -> &'static [FrameRange] {
    let state = memory_ref();
    // SAFETY: usable ranges are immutable after memory initialization.
    unsafe { core::slice::from_raw_parts(state.usable_ranges.as_ptr(), state.usable_range_count) }
}

pub fn heap_range() -> Option<FrameRange> {
    memory_ref().heap_range
}

fn run_allocator_self_check(state: &mut MemoryState) -> Result<()> {
    let first = state
        .allocator
        .alloc_frame()
        .ok_or(MemoryError::NoUsableRam)?;
    let second = state
        .allocator
        .alloc_frame()
        .ok_or(MemoryError::NoUsableRam)?;
    let third = state
        .allocator
        .alloc_frame()
        .ok_or(MemoryError::NoUsableRam)?;

    crate::debug!("memory: self-check alloc frames {first}, {second}, {third}");

    state.allocator.free_frame(third);
    state.allocator.free_frame(second);
    state.allocator.free_frame(first);
    crate::debug!("memory: self-check free frames restored");
    Ok(())
}

fn is_valid_usable_frame(usable_ranges: &[FrameRange], frame_pa: PhysAddr) -> bool {
    if frame_pa & (PAGE_SIZE - 1) != 0 {
        return false;
    }

    usable_ranges
        .iter()
        .any(|range| frame_pa >= range.start && frame_pa < range.end)
}

fn is_valid_usable_frame_range(usable_ranges: &[FrameRange], range: FrameRange) -> bool {
    if range.start >= range.end {
        return false;
    }
    if range.start & (PAGE_SIZE - 1) != 0 || range.end & (PAGE_SIZE - 1) != 0 {
        return false;
    }

    usable_ranges
        .iter()
        .any(|usable| range.start >= usable.start && range.end <= usable.end)
}

fn kernel_image_range() -> PhysRange {
    PhysRange {
        start: unsafe { core::ptr::addr_of!(__kernel_image_phys_start_value).read_volatile() },
        end: unsafe { core::ptr::addr_of!(__kernel_image_phys_end_value).read_volatile() },
    }
}

fn boot_stack_range() -> PhysRange {
    PhysRange {
        start: unsafe { core::ptr::addr_of!(__boot_stack_bottom_value).read_volatile() },
        end: unsafe { core::ptr::addr_of!(__boot_stack_top_value).read_volatile() },
    }
}

fn dtb_range(boot: &BootInfo) -> Result<Option<PhysRange>> {
    phys_range_from_u64(boot.dtb_pa, boot.dtb_size)
}

#[inline(always)]
fn phys_to_kernel_va(pa: PhysAddr) -> VirtAddr {
    unsafe { arch_phys_to_virt(pa) }
}

#[inline(always)]
fn phys_to_kernel_ptr<T>(pa: PhysAddr) -> *mut T {
    phys_to_kernel_va(pa) as *mut T
}

#[inline(always)]
fn memory_mut() -> &'static mut MemoryState {
    // SAFETY: genrt mutates memory state only on one core during bring-up.
    unsafe { &mut *MEMORY.0.get() }
}

#[inline(always)]
fn memory_ref() -> &'static MemoryState {
    // SAFETY: read-only access does not outlive the static backing storage.
    unsafe { &*MEMORY.0.get() }
}
