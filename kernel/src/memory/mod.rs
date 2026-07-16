use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use bootinfo::BootInfo;

use crate::sync::PreemptLock;

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
pub(crate) use types::{align_down, align_up};

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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FrameRangeCloneError {
    InvalidRange,
    OutOfFrames,
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

struct MemoryMetadata {
    phys_regions: [PhysRegion; MAX_PHYS_REGIONS],
    phys_region_count: usize,
    usable_ranges: [FrameRange; MAX_USABLE_RANGES],
    usable_range_count: usize,
    heap_range: Option<FrameRange>,
}

impl MemoryMetadata {
    const fn new() -> Self {
        Self {
            phys_regions: [PhysRegion {
                range: PhysRange::empty(),
                kind: RegionKind::Reserved,
            }; MAX_PHYS_REGIONS],
            phys_region_count: 0,
            usable_ranges: [FrameRange::empty(); MAX_USABLE_RANGES],
            usable_range_count: 0,
            heap_range: None,
        }
    }

    fn reset(&mut self) {
        self.phys_region_count = 0;
        self.usable_range_count = 0;
        self.heap_range = None;
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

struct MemoryMetadataCell(UnsafeCell<MemoryMetadata>);

// SAFETY: metadata is written only during single-core boot before scheduler
// entry and remains immutable for the rest of runtime.
unsafe impl Sync for MemoryMetadataCell {}

static MEMORY_METADATA: MemoryMetadataCell =
    MemoryMetadataCell(UnsafeCell::new(MemoryMetadata::new()));
static MEMORY_METADATA_READY: AtomicBool = AtomicBool::new(false);

struct RuntimeFrameAllocator {
    initialized: bool,
    allocator: FrameAllocator<PhysAddr, PhysFrameStorage>,
}

impl RuntimeFrameAllocator {
    const fn new() -> Self {
        Self {
            initialized: false,
            allocator: FrameAllocator::new(),
        }
    }

    fn reset(&mut self) {
        self.initialized = false;
        self.allocator.reset();
    }
}

static FRAME_ALLOCATOR: PreemptLock<RuntimeFrameAllocator> =
    PreemptLock::new(RuntimeFrameAllocator::new());

/// Initialize immutable physical-memory metadata and the runtime allocators.
///
/// This boot-only operation normalizes firmware ranges, reserves kernel-owned
/// regions, initializes the task-only frame allocator, extracts the fixed heap
/// range, and initializes the kernel heap. It runs before scheduler entry.
///
/// # Arguments
///
/// * `boot` - Immutable boot information containing the firmware memory map and
///   optional DTB range.
///
/// # Returns
///
/// Returns `Ok(())` after publishing immutable metadata and initialized frame
/// and heap allocators.
///
/// # Errors
///
/// Returns [`MemoryError`] for unusable or over-capacity boot maps, invalid
/// addresses, unavailable heap frames, heap initialization failure, or a failed
/// heap smoke check.
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

    let metadata = metadata_mut();
    metadata.reset();

    build_memory_map(
        &ram_ranges[..ram_count],
        &reserved_ranges[..reserved_count],
        &mut metadata.phys_regions,
        &mut metadata.phys_region_count,
        &mut metadata.usable_ranges,
        &mut metadata.usable_range_count,
    )?;

    let usable = &metadata.usable_ranges[..metadata.usable_range_count];
    if usable.is_empty() {
        return Err(MemoryError::NoUsableRam);
    }

    for region in &metadata.phys_regions[..metadata.phys_region_count] {
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

    let bootstrap_heap_range = {
        let mut runtime = FRAME_ALLOCATOR.lock();
        runtime.reset();
        runtime.allocator.init_from_ranges(usable, PAGE_SIZE);

        // The bootstrap heap is allocated from the frame allocator before the
        // rest of the kernel starts using heap-backed containers. Ownership is
        // transferred to the heap subsystem even though `usable_ranges()`
        // continues to describe the broader immutable usable RAM set.
        crate::debug!("memory: allocating bootstrap heap from frame allocator");
        let bootstrap_heap_range = runtime
            .allocator
            .alloc_contiguous(KERNEL_HEAP_BOOTSTRAP_SIZE / PAGE_SIZE, PAGE_SIZE)
            .ok_or(MemoryError::NoBootstrapHeapRange)?;
        run_allocator_self_check(&mut runtime.allocator)?;
        runtime.initialized = true;
        bootstrap_heap_range
    };
    crate::debug!(
        "memory: bootstrap heap allocated {:?}",
        bootstrap_heap_range
    );
    metadata.heap_range = Some(bootstrap_heap_range);
    let usable_range_count = metadata.usable_range_count;

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
    MEMORY_METADATA_READY.store(true, Ordering::Release);

    crate::info!(
        "memory: initialized usable_ranges={} free_frames={} heap_kib={} heap_phys_range={:?} heap_virt_range={:?}",
        usable_range_count,
        free_frame_count().unwrap_or(0),
        KERNEL_HEAP_BOOTSTRAP_SIZE / 1024,
        bootstrap_heap_range,
        VirtRange {
            start: bootstrap_heap_va,
            end: bootstrap_heap_va + (bootstrap_heap_range.end - bootstrap_heap_range.start),
        }
    );
    Ok(())
}

/// Allocate one physical frame from the runtime free list.
///
/// # Returns
///
/// Returns the physical address of one page-aligned frame, or `None` when the
/// allocator is not initialized or no frame remains. The task-only allocator
/// lock is held for free-list mutation; the operation does not allocate from
/// the kernel heap or block.
pub fn alloc_frame() -> Option<PhysAddr> {
    let mut runtime = FRAME_ALLOCATOR.lock();
    if !runtime.initialized {
        return None;
    }

    runtime.allocator.alloc_frame()
}

/// Allocate one contiguous physical frame range.
///
/// # Arguments
///
/// * `frames` - Number of contiguous page-sized frames to allocate.
///
/// # Returns
///
/// Returns the allocated page-aligned range, or `None` for zero frames, an
/// uninitialized allocator, or insufficient contiguous space. The task-only
/// allocator lock is held during free-list traversal; the operation does not
/// allocate from the kernel heap or block.
pub fn alloc_contiguous_frames(frames: usize) -> Option<FrameRange> {
    let mut runtime = FRAME_ALLOCATOR.lock();
    if !runtime.initialized {
        return None;
    }

    runtime.allocator.alloc_contiguous(frames, PAGE_SIZE)
}

/// Return one physical frame to the runtime free list.
///
/// # Arguments
///
/// * `frame_pa` - Page-aligned physical address previously returned by the
///   frame allocator.
///
/// # Returns
///
/// Returns after reinserting the frame under the task-only allocator lock. The
/// operation does not allocate from the kernel heap or block.
///
/// # Panics
///
/// Panics before allocator initialization or when `frame_pa` is unaligned or
/// outside immutable boot-discovered usable RAM.
pub fn free_frame(frame_pa: PhysAddr) {
    let mut runtime = FRAME_ALLOCATOR.lock();
    if !runtime.initialized {
        panic!("memory: free_frame before init");
    }
    if !is_valid_usable_frame(usable_ranges(), frame_pa) {
        panic!("memory: attempted to free invalid frame {frame_pa}");
    }

    runtime.allocator.free_frame(frame_pa);
}

/// Return a contiguous physical frame range to the runtime free list.
///
/// # Arguments
///
/// * `range` - Non-empty page-aligned range previously returned by the frame
///   allocator.
///
/// # Returns
///
/// Returns after reinserting the complete range under the task-only allocator
/// lock. The operation does not allocate from the kernel heap or block.
///
/// # Panics
///
/// Panics before allocator initialization or when `range` is empty, unaligned,
/// or not wholly contained in one immutable boot-discovered usable RAM range.
pub fn free_contiguous_frames(range: FrameRange) {
    let mut runtime = FRAME_ALLOCATOR.lock();
    if !runtime.initialized {
        panic!("memory: free_contiguous_frames before init");
    }
    if !is_valid_usable_frame_range(usable_ranges(), range) {
        panic!("memory: attempted to free invalid frame range {:?}", range);
    }

    runtime.allocator.free_contiguous(range, PAGE_SIZE);
}

/// Return the current number of free physical frames.
///
/// # Returns
///
/// Returns `Some(count)` after runtime allocator initialization, or `None`
/// before initialization. The count is copied while holding the task-only
/// allocator lock; no protected reference escapes, and the operation does not
/// allocate or block.
pub(crate) fn free_frame_count() -> Option<usize> {
    let runtime = FRAME_ALLOCATOR.lock();
    runtime.initialized.then(|| runtime.allocator.free_frames())
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

pub(crate) fn copy_bytes_to_phys(dst_pa: PhysAddr, src: &[u8]) {
    if src.is_empty() {
        return;
    }

    // SAFETY: callers pass physical RAM ranges that are owned by the caller and
    // reachable through the kernel direct map. Keeping the dereference here
    // preserves the frame allocator's physical-address-only contract.
    unsafe {
        core::ptr::copy_nonoverlapping(
            src.as_ptr(),
            phys_to_kernel_va(dst_pa) as *mut u8,
            src.len(),
        )
    };
}

pub(crate) fn clone_frame_range(
    src: FrameRange,
) -> core::result::Result<FrameRange, FrameRangeCloneError> {
    let size = src
        .end
        .checked_sub(src.start)
        .ok_or(FrameRangeCloneError::InvalidRange)?;
    if size == 0 || src.start & (PAGE_SIZE - 1) != 0 || src.end & (PAGE_SIZE - 1) != 0 {
        return Err(FrameRangeCloneError::InvalidRange);
    }

    let dst = alloc_contiguous_frames(size / PAGE_SIZE).ok_or(FrameRangeCloneError::OutOfFrames)?;
    copy_phys_range(src.start, dst.start, size);
    Ok(dst)
}

fn copy_phys_range(src_pa: PhysAddr, dst_pa: PhysAddr, size: usize) {
    if size == 0 {
        return;
    }

    // SAFETY: callers pass valid physical RAM ranges. This module is the memory
    // boundary that converts PA to a direct-map HVA for the actual copy.
    unsafe {
        core::ptr::copy_nonoverlapping(
            phys_to_kernel_va(src_pa) as *const u8,
            phys_to_kernel_va(dst_pa) as *mut u8,
            size,
        );
    }
}

/// Borrow the immutable boot-discovered usable physical ranges.
///
/// # Returns
///
/// Returns the initialized prefix of static memory metadata. Before memory
/// initialization the slice is empty. No runtime allocator lock is acquired,
/// and the returned ranges never change after boot.
pub fn usable_ranges() -> &'static [FrameRange] {
    if !MEMORY_METADATA_READY.load(Ordering::Acquire) {
        return &[];
    }
    let metadata = metadata_ref();
    // SAFETY: usable ranges are immutable after memory initialization.
    unsafe {
        core::slice::from_raw_parts(metadata.usable_ranges.as_ptr(), metadata.usable_range_count)
    }
}

/// Return the physical range permanently assigned to the fixed kernel heap.
///
/// # Returns
///
/// Returns `Some(range)` after the bootstrap heap has been extracted from the
/// frame allocator, or `None` before that point. The value comes from immutable
/// boot metadata and requires no runtime allocator lock.
pub fn heap_range() -> Option<FrameRange> {
    if !MEMORY_METADATA_READY.load(Ordering::Acquire) {
        return None;
    }
    metadata_ref().heap_range
}

fn run_allocator_self_check(
    allocator: &mut FrameAllocator<PhysAddr, PhysFrameStorage>,
) -> Result<()> {
    let first = allocator.alloc_frame().ok_or(MemoryError::NoUsableRam)?;
    let second = allocator.alloc_frame().ok_or(MemoryError::NoUsableRam)?;
    let third = allocator.alloc_frame().ok_or(MemoryError::NoUsableRam)?;

    crate::debug!("memory: self-check alloc frames {first}, {second}, {third}");

    allocator.free_frame(third);
    allocator.free_frame(second);
    allocator.free_frame(first);
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
fn metadata_mut() -> &'static mut MemoryMetadata {
    // SAFETY: boot initialization is the sole writer and completes before any
    // runtime task can observe immutable memory metadata.
    unsafe { &mut *MEMORY_METADATA.0.get() }
}

#[inline(always)]
fn metadata_ref() -> &'static MemoryMetadata {
    // SAFETY: metadata is immutable after boot initialization and has static
    // backing storage.
    unsafe { &*MEMORY_METADATA.0.get() }
}
