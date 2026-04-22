# ADR-0009: Bootstrap Kernel Heap on Top of Contiguous Frame Allocation

## Status
Accepted

## Context

`genrt` already had:

* DTB-backed physical memory discovery
* internal physical memory map normalization
* a free-list physical frame allocator

The next milestone required a working global heap allocator so the kernel could
use `alloc` containers such as `Vec`, `VecDeque`, `BinaryHeap`, and `BTreeMap`
without introducing a custom general-purpose heap implementation.

`linked_list_allocator` expects one contiguous heap region. For this stage we do
not want a growable heap assembled from arbitrary frames, nor a slab/buddy
allocator.

## Decision

The kernel now uses a fixed-size bootstrap heap implemented with
`linked_list_allocator::Heap` behind a kernel-owned wrapper.

Key points:

* heap code lives under `kernel::memory::heap`
* the bootstrap heap size is fixed at `16 MiB`
* the heap is initialized once from a contiguous range returned by the existing
  frame allocator via `alloc_contiguous`
* the heap region is therefore removed from the frame allocator free list before
  general kernel code starts using `alloc`
* allocator locking and IRQ-reentrancy policy are documented separately in
  ADR-0010
* runtime smoke tests validate `Vec`, `VecDeque`, `BinaryHeap<Reverse<_>>`, and
  `BTreeMap`

Initialization order is:

1. parse and normalize the physical memory map
2. initialize the frame allocator on usable page ranges
3. allocate one contiguous `16 MiB` heap region from the frame allocator
4. initialize `linked_list_allocator`
5. run heap-backed smoke tests
6. continue with the rest of kernel bootstrap

OOM currently follows the default `alloc` panic path, which converges into the
existing kernel panic -> architecture hard-fault termination policy.

## Consequences

Positive:

* minimal implementation effort
* immediate support for standard `alloc` containers in a `no_std` kernel
* no need to design a custom heap allocator yet
* heap ownership remains unambiguous because the region is allocated out of the
  frame allocator before later consumers use it

Limitations:

* heap size is fixed
* heap growth from arbitrary frames is not implemented
* `usable_ranges()` still describes the broader usable RAM set, while actual
  heap ownership is tracked separately through the allocated heap range and the
  frame allocator state
