use alloc::{
    collections::{BTreeMap, BinaryHeap, VecDeque},
    vec::Vec,
};
use core::{
    alloc::{GlobalAlloc, Layout},
    cmp::Reverse,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use linked_list_allocator::Heap;

use crate::sync::PreemptLock;

// Heap allocation policy for the current single-core kernel:
// - allowed during bootstrap/init code,
// - allowed in ordinary task context,
// - protected against task preemption by the task-only allocator lock,
// - still forbidden in timer/scheduler/exception fast paths.
//
// The current transitional PreemptLock backend masks local IRQs. This preserves
// runtime behavior while keeping task-only ownership distinct from state that
// is intentionally shared with interrupt handlers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HeapInitError {
    AlreadyInitialized,
    ZeroSize,
    UnalignedRange,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HeapSmokeError {
    Vec,
    VecDeque,
    BinaryHeap,
    BTreeMap,
}

struct KernelHeap {
    heap: PreemptLock<Heap>,
    initialized: AtomicBool,
}

// SAFETY: allocator state is shared globally, but access is serialized through
// the task-only preemption lock. Allocation from IRQ context remains forbidden.
unsafe impl Sync for KernelHeap {}

#[global_allocator]
static KERNEL_HEAP: KernelHeap = KernelHeap::empty();

// OOM currently follows the default alloc panic path, which then converges
// into genrt's existing panic -> arch_hard_fault termination policy.

impl KernelHeap {
    const fn empty() -> Self {
        Self {
            heap: PreemptLock::new(Heap::empty()),
            initialized: AtomicBool::new(false),
        }
    }

    fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    unsafe fn init(&self, heap_start: usize, heap_size: usize) -> Result<(), HeapInitError> {
        if self.is_initialized() {
            return Err(HeapInitError::AlreadyInitialized);
        }

        let mut guard = self.heap.lock();
        if !guard.bottom().is_null() {
            return Err(HeapInitError::AlreadyInitialized);
        }

        // SAFETY: the caller guarantees that `heap_start..heap_start + heap_size`
        // is a unique, page-aligned, contiguous bootstrap heap region allocated
        // from the frame allocator before other kernel users receive heap access.
        unsafe {
            guard.init(heap_start as *mut u8, heap_size);
        }
        self.initialized.store(true, Ordering::Release);
        Ok(())
    }
}

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut guard = self.heap.lock();
        guard
            .allocate_first_fit(layout)
            .ok()
            .map_or(core::ptr::null_mut(), |allocation| allocation.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut guard = self.heap.lock();
        // SAFETY: `GlobalAlloc` callers pass back exactly the pointer/layout pair that
        // originated from this allocator instance.
        unsafe { guard.deallocate(NonNull::new_unchecked(ptr), layout) }
    }
}

pub fn init_heap(heap_start: usize, heap_size: usize) -> Result<(), HeapInitError> {
    if heap_size == 0 {
        return Err(HeapInitError::ZeroSize);
    }
    if heap_start & (super::PAGE_SIZE - 1) != 0 || heap_size & (super::PAGE_SIZE - 1) != 0 {
        return Err(HeapInitError::UnalignedRange);
    }

    // SAFETY: `memory::init()` allocates one unique contiguous heap range from the
    // frame allocator before handing general kernel code access to `alloc`.
    unsafe { KERNEL_HEAP.init(heap_start, heap_size)? };
    crate::info!(
        "heap: initialized start={} size={} KiB",
        heap_start,
        heap_size / 1024
    );
    Ok(())
}

pub fn heap_is_initialized() -> bool {
    KERNEL_HEAP.is_initialized()
}

pub fn run_heap_smoke_tests() -> Result<(), HeapSmokeError> {
    run_vec_smoke_test()?;
    run_vecdeque_smoke_test()?;
    run_binary_heap_smoke_test()?;
    run_btree_map_smoke_test()?;
    crate::info!("heap: alloc smoke tests passed");
    Ok(())
}

fn run_vec_smoke_test() -> Result<(), HeapSmokeError> {
    let mut values = Vec::new();
    values.push(3u64);
    values.push(5u64);
    values.push(8u64);
    values.push(13u64);

    if values.as_slice() == [3, 5, 8, 13] {
        Ok(())
    } else {
        Err(HeapSmokeError::Vec)
    }
}

fn run_vecdeque_smoke_test() -> Result<(), HeapSmokeError> {
    let mut queue = VecDeque::new();
    queue.push_back(10u32);
    queue.push_back(20u32);
    queue.push_back(30u32);

    if queue.pop_front() != Some(10) {
        return Err(HeapSmokeError::VecDeque);
    }
    if queue.pop_front() != Some(20) {
        return Err(HeapSmokeError::VecDeque);
    }
    if queue.pop_front() != Some(30) {
        return Err(HeapSmokeError::VecDeque);
    }
    if !queue.is_empty() {
        return Err(HeapSmokeError::VecDeque);
    }
    Ok(())
}

fn run_binary_heap_smoke_test() -> Result<(), HeapSmokeError> {
    let mut heap = BinaryHeap::new();
    heap.push(Reverse(7u64));
    heap.push(Reverse(2u64));
    heap.push(Reverse(11u64));
    heap.push(Reverse(5u64));

    if heap.pop() != Some(Reverse(2)) {
        return Err(HeapSmokeError::BinaryHeap);
    }
    if heap.pop() != Some(Reverse(5)) {
        return Err(HeapSmokeError::BinaryHeap);
    }
    if heap.pop() != Some(Reverse(7)) {
        return Err(HeapSmokeError::BinaryHeap);
    }
    if heap.pop() != Some(Reverse(11)) {
        return Err(HeapSmokeError::BinaryHeap);
    }
    if !heap.is_empty() {
        return Err(HeapSmokeError::BinaryHeap);
    }
    Ok(())
}

fn run_btree_map_smoke_test() -> Result<(), HeapSmokeError> {
    let mut map = BTreeMap::new();
    map.insert(1u64, 100u64);
    map.insert(2u64, 200u64);
    map.insert(3u64, 300u64);

    if map.get(&2) != Some(&200) {
        return Err(HeapSmokeError::BTreeMap);
    }
    if map.remove(&2) != Some(200) {
        return Err(HeapSmokeError::BTreeMap);
    }
    if map.contains_key(&2) {
        return Err(HeapSmokeError::BTreeMap);
    }
    Ok(())
}
