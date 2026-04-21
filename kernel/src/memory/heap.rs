use alloc::{
    collections::{BTreeMap, BinaryHeap, VecDeque},
    vec::Vec,
};
use core::cmp::Reverse;

use linked_list_allocator::LockedHeap;

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

#[global_allocator]
static KERNEL_HEAP: LockedHeap = LockedHeap::empty();

// OOM currently follows the default alloc panic path, which then converges
// into genrt's existing panic -> arch_hard_fault termination policy.

pub fn init_heap(heap_start: usize, heap_size: usize) -> Result<(), HeapInitError> {
    if heap_is_initialized() {
        return Err(HeapInitError::AlreadyInitialized);
    }
    if heap_size == 0 {
        return Err(HeapInitError::ZeroSize);
    }
    if heap_start & (super::PAGE_SIZE - 1) != 0 || heap_size & (super::PAGE_SIZE - 1) != 0 {
        return Err(HeapInitError::UnalignedRange);
    }

    // SAFETY: the caller guarantees that `heap_start..heap_start + heap_size`
    // is a unique, page-aligned, contiguous bootstrap heap region allocated
    // from the frame allocator before other kernel users receive heap access.
    unsafe {
        KERNEL_HEAP.lock().init(heap_start as *mut u8, heap_size);
    }
    crate::info!(
        "heap: initialized start={} size={} KiB",
        heap_start,
        heap_size / 1024
    );
    Ok(())
}

pub fn heap_is_initialized() -> bool {
    !KERNEL_HEAP.lock().bottom().is_null()
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
