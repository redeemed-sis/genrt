use alloc::{
    collections::{BTreeMap, BinaryHeap, VecDeque},
    vec::Vec,
};
use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    cmp::Reverse,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use linked_list_allocator::Heap;

unsafe extern "C" {
    fn arch_local_irq_save_and_disable() -> u64;
    fn arch_local_irq_restore(saved_daif: u64);
}

// Heap allocation policy for the current single-core kernel:
// - allowed during bootstrap/init code,
// - allowed in ordinary task context,
// - protected against local IRQ reentrancy by saving/restoring the local IRQ mask,
// - still forbidden in timer/scheduler/exception fast paths.
//
// The lock below is intentionally minimal and single-core scoped. It prevents a
// task-context allocation from being interrupted by the timer IRQ and re-entering
// the allocator, but it is not an SMP-safe synchronization primitive.
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

struct IrqSafeHeap {
    heap: UnsafeCell<Heap>,
    locked: AtomicBool,
    initialized: AtomicBool,
}

struct LocalIrqGuard {
    saved_daif: u64,
}

struct HeapGuard<'a> {
    owner: &'a IrqSafeHeap,
    _irq_guard: LocalIrqGuard,
}

// SAFETY: allocator state is shared globally, but access is serialized through
// the single-core IRQ-save lock above.
unsafe impl Sync for IrqSafeHeap {}

#[global_allocator]
static KERNEL_HEAP: IrqSafeHeap = IrqSafeHeap::empty();

// OOM currently follows the default alloc panic path, which then converges
// into genrt's existing panic -> arch_hard_fault termination policy.

impl IrqSafeHeap {
    const fn empty() -> Self {
        Self {
            heap: UnsafeCell::new(Heap::empty()),
            locked: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> HeapGuard<'_> {
        let irq_guard = LocalIrqGuard::save_and_disable();
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            drop(irq_guard);
            panic!("heap: recursive or IRQ-reentrant allocator entry");
        }

        HeapGuard {
            owner: self,
            _irq_guard: irq_guard,
        }
    }

    fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    unsafe fn init(&self, heap_start: usize, heap_size: usize) -> Result<(), HeapInitError> {
        if self.is_initialized() {
            return Err(HeapInitError::AlreadyInitialized);
        }

        let mut guard = self.lock();
        if !guard.heap_mut().bottom().is_null() {
            return Err(HeapInitError::AlreadyInitialized);
        }

        // SAFETY: the caller guarantees that `heap_start..heap_start + heap_size`
        // is a unique, page-aligned, contiguous bootstrap heap region allocated
        // from the frame allocator before other kernel users receive heap access.
        unsafe {
            guard.heap_mut().init(heap_start as *mut u8, heap_size);
        }
        self.initialized.store(true, Ordering::Release);
        Ok(())
    }
}

impl LocalIrqGuard {
    #[inline(always)]
    fn save_and_disable() -> Self {
        // SAFETY: the architecture layer returns the current local DAIF state and masks
        // IRQ delivery on this single core until `Drop` restores the saved state.
        let saved_daif = unsafe { arch_local_irq_save_and_disable() };
        Self { saved_daif }
    }
}

impl Drop for LocalIrqGuard {
    fn drop(&mut self) {
        // SAFETY: `saved_daif` came from `arch_local_irq_save_and_disable()` on the
        // same core, so restoring it here returns the caller to its prior IRQ state.
        unsafe { arch_local_irq_restore(self.saved_daif) }
    }
}

impl HeapGuard<'_> {
    #[inline(always)]
    fn heap_mut(&mut self) -> &mut Heap {
        // SAFETY: the IRQ-save lock guarantees exclusive access to the allocator state
        // on the current single core for the guard's lifetime.
        unsafe { &mut *self.owner.heap.get() }
    }
}

impl Drop for HeapGuard<'_> {
    fn drop(&mut self) {
        self.owner.locked.store(false, Ordering::Release);
    }
}

unsafe impl GlobalAlloc for IrqSafeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut guard = self.lock();
        guard
            .heap_mut()
            .allocate_first_fit(layout)
            .ok()
            .map_or(core::ptr::null_mut(), |allocation| allocation.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut guard = self.lock();
        // SAFETY: `GlobalAlloc` callers pass back exactly the pointer/layout pair that
        // originated from this allocator instance.
        unsafe {
            guard
                .heap_mut()
                .deallocate(NonNull::new_unchecked(ptr), layout)
        }
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
