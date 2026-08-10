use core::marker::PhantomData;

use super::types::AddrRange;

pub(crate) trait FreeListAddr: Copy + Eq + Ord {
    fn from_usize(value: usize) -> Self;
    fn to_usize(self) -> usize;
}

impl FreeListAddr for usize {
    #[inline(always)]
    fn from_usize(value: usize) -> Self {
        value
    }

    #[inline(always)]
    fn to_usize(self) -> usize {
        self
    }
}

pub(crate) trait FreeListStorage<A: FreeListAddr> {
    fn free_list_end() -> A;
    unsafe fn read_next_free_frame(frame: A) -> A;
    unsafe fn write_next_free_frame(frame: A, next: A);
}

#[derive(Copy, Clone)]
pub(crate) struct FrameAllocator<A, S> {
    head: Option<A>,
    page_size: usize,
    total_frames: usize,
    free_frames: usize,
    storage: PhantomData<S>,
}

impl<A, S> FrameAllocator<A, S>
where
    A: FreeListAddr,
    S: FreeListStorage<A>,
{
    pub(crate) const fn new() -> Self {
        Self {
            head: None,
            page_size: 0,
            total_frames: 0,
            free_frames: 0,
            storage: PhantomData,
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn init_from_ranges(&mut self, ranges: &[AddrRange<A>], page_size: usize) {
        self.reset();
        self.page_size = page_size;

        for range in ranges.iter().rev() {
            let mut frame = range.end;
            while frame > range.start {
                frame = A::from_usize(frame.to_usize() - page_size);
                self.insert_frame_sorted(frame);
            }
        }

        self.total_frames = self.free_frames;
    }

    pub(crate) fn alloc_frame(&mut self) -> Option<A> {
        let frame = self.head?;
        // SAFETY: storage policy owns how free-list metadata is stored for `A`.
        let next = unsafe { S::read_next_free_frame(frame) };
        self.head = (next != S::free_list_end()).then_some(next);
        self.free_frames = self.free_frames.saturating_sub(1);
        Some(frame)
    }

    pub(crate) fn alloc_contiguous(
        &mut self,
        frames: usize,
        page_size: usize,
    ) -> Option<AddrRange<A>> {
        self.alloc_contiguous_aligned(frames, page_size, page_size)
    }

    pub(crate) fn alloc_contiguous_aligned(
        &mut self,
        frames: usize,
        page_size: usize,
        align: usize,
    ) -> Option<AddrRange<A>> {
        if frames == 0 {
            return None;
        }
        if align < page_size || !align.is_power_of_two() {
            return None;
        }

        let mut prev: Option<A> = None;
        let mut current = self.head;
        let mut previous_frame: Option<A> = None;
        let mut candidate_prev: Option<A> = None;
        let mut run_start: Option<A> = None;
        let mut run_end: Option<A> = None;
        let mut run_len = 0usize;

        while let Some(frame) = current {
            // SAFETY: storage policy owns how free-list metadata is stored for `A`.
            let next_raw = unsafe { S::read_next_free_frame(frame) };
            let next = (next_raw != S::free_list_end()).then_some(next_raw);

            let contiguous = match previous_frame {
                Some(last) if frame.to_usize() == last.to_usize() + page_size => true,
                _ => false,
            };
            if !contiguous {
                candidate_prev = None;
                run_start = None;
                run_end = None;
                run_len = 0;
            }

            if run_start.is_none() && frame.to_usize() & (align - 1) == 0 {
                candidate_prev = prev;
                run_start = Some(frame);
                run_end = Some(frame);
                run_len = 1;
            } else if run_start.is_some() {
                run_end = Some(frame);
                run_len += 1;
            }

            if run_len == frames {
                if let Some(before_run) = candidate_prev {
                    // SAFETY: `before_run` remains in the free list; we splice the
                    // aligned contiguous run out by redirecting its next pointer.
                    unsafe {
                        S::write_next_free_frame(before_run, next.unwrap_or_else(S::free_list_end))
                    };
                } else {
                    self.head = next;
                }

                self.free_frames = self.free_frames.saturating_sub(frames);
                let start = run_start.expect("contiguous run start must be set");
                let end = A::from_usize(
                    run_end
                        .expect("contiguous run end must be set")
                        .to_usize()
                        .checked_add(page_size)
                        .expect("frame range end overflow"),
                );
                return Some(AddrRange { start, end });
            }

            prev = Some(frame);
            previous_frame = Some(frame);
            current = next;
        }

        None
    }

    pub(crate) fn free_frame(&mut self, frame: A) {
        debug_assert!(self.page_size != 0);
        self.free_contiguous(
            AddrRange {
                start: frame,
                end: A::from_usize(
                    frame
                        .to_usize()
                        .checked_add(self.page_size)
                        .expect("single-frame free end overflow"),
                ),
            },
            self.page_size,
        );
    }

    pub(crate) fn free_contiguous(&mut self, range: AddrRange<A>, page_size: usize) {
        let start = range.start.to_usize();
        let end = range.end.to_usize();
        if start >= end {
            return;
        }

        debug_assert_eq!(start % page_size, 0);
        debug_assert_eq!(end % page_size, 0);

        let frame_count = (end - start) / page_size;
        debug_assert!(frame_count > 0);

        let mut prev: Option<A> = None;
        let mut current = self.head;

        while let Some(frame) = current {
            if frame >= range.start {
                break;
            }
            prev = Some(frame);
            // SAFETY: storage policy owns how free-list metadata is stored for `A`.
            let next_raw = unsafe { S::read_next_free_frame(frame) };
            current = (next_raw != S::free_list_end()).then_some(next_raw);
        }

        debug_assert!(prev.is_none_or(|frame| frame < range.start));
        debug_assert!(current.is_none_or(|frame| frame >= range.end));

        let tail = current.unwrap_or_else(S::free_list_end);
        let mut frame = end;
        let mut next = tail;
        while frame > start {
            frame -= page_size;
            let current_frame = A::from_usize(frame);
            // SAFETY: we are constructing free-list links inside the newly freed
            // contiguous block before splicing it back into the global list.
            unsafe { S::write_next_free_frame(current_frame, next) };
            next = current_frame;
        }

        if let Some(before_range) = prev {
            // SAFETY: `before_range` remains in the free list and is updated to
            // point at the first frame in the inserted contiguous block.
            unsafe { S::write_next_free_frame(before_range, range.start) };
        } else {
            self.head = Some(range.start);
        }

        self.free_frames += frame_count;
    }

    pub(crate) fn free_frames(&self) -> usize {
        self.free_frames
    }

    fn insert_frame_sorted(&mut self, frame: A) {
        let Some(head) = self.head else {
            // SAFETY: `frame` becomes the only node in the list.
            unsafe { S::write_next_free_frame(frame, S::free_list_end()) };
            self.head = Some(frame);
            self.free_frames += 1;
            return;
        };

        if frame < head {
            // SAFETY: `frame` becomes the new list head and points to the old head.
            unsafe { S::write_next_free_frame(frame, head) };
            self.head = Some(frame);
            self.free_frames += 1;
            return;
        }

        let mut prev = head;
        loop {
            // SAFETY: storage policy owns how free-list metadata is stored for `A`.
            let next_raw = unsafe { S::read_next_free_frame(prev) };
            let next = (next_raw != S::free_list_end()).then_some(next_raw);

            if next.is_none_or(|candidate| frame < candidate) {
                // SAFETY: insert `frame` between `prev` and `next`, preserving the
                // allocator invariant that the free list stays sorted by address.
                unsafe {
                    S::write_next_free_frame(frame, next.unwrap_or_else(S::free_list_end));
                    S::write_next_free_frame(prev, frame);
                }
                self.free_frames += 1;
                return;
            }

            prev = next.expect("next free frame must exist while traversing list");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::SpinLock;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::{sync::Arc, thread, vec::Vec};

    const TEST_PAGE_SIZE: usize = 4096;
    const TEST_FRAMES: usize = 128;
    static NEXT: [AtomicUsize; TEST_FRAMES] = [const { AtomicUsize::new(usize::MAX) }; TEST_FRAMES];

    struct AtomicStorage;

    impl FreeListStorage<usize> for AtomicStorage {
        fn free_list_end() -> usize {
            usize::MAX
        }

        unsafe fn read_next_free_frame(frame: usize) -> usize {
            NEXT[frame / TEST_PAGE_SIZE].load(Ordering::Acquire)
        }

        unsafe fn write_next_free_frame(frame: usize, next: usize) {
            NEXT[frame / TEST_PAGE_SIZE].store(next, Ordering::Release);
        }
    }

    #[test]
    fn spin_locked_allocator_survives_host_contention() {
        let allocator = Arc::new(SpinLock::new(FrameAllocator::<usize, AtomicStorage>::new()));
        allocator.lock().init_from_ranges(
            &[AddrRange {
                start: 0,
                end: TEST_FRAMES * TEST_PAGE_SIZE,
            }],
            TEST_PAGE_SIZE,
        );

        let workers: Vec<_> = (0..4)
            .map(|_| {
                let allocator = Arc::clone(&allocator);
                thread::spawn(move || {
                    for _ in 0..1_000 {
                        let frame = loop {
                            if let Some(frame) = allocator.lock().alloc_frame() {
                                break frame;
                            }
                            thread::yield_now();
                        };
                        allocator.lock().free_frame(frame);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(allocator.lock().free_frames(), TEST_FRAMES);
    }
}
