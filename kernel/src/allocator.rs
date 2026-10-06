use linked_list_allocator::LockedHeap;
use x86_64::structures::paging::{
    mapper::MapToError, FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
};
use x86_64::VirtAddr;

/// Chosen arbitrarily high in canonical address space, well clear of the
/// bootloader's own mappings (physical memory offset, kernel image, boot
/// info) — the exact value doesn't matter, only that nothing else claims
/// this virtual range. Matches the "Writing an OS in Rust" tutorial's pick.
pub const HEAP_START: usize = 0x_4444_4444_0000;

/// 100 KiB: enough to exercise a real allocator (multiple `Vec`/`Box`
/// allocations, freeing and reuse) without costing much physical memory or
/// mapping time. The heap is fixed-size and never grows; `init_heap` maps
/// exactly this much and nothing resizes it afterward.
pub const HEAP_SIZE: usize = 100 * 1024;

/// `linked_list_allocator`'s `LockedHeap` is a `spin::Mutex` around the free
/// list internally, so unlike `MAPPER`/`FRAME_ALLOCATOR` it doesn't need a
/// separate `OnceCell` wrapper — `empty()` is a valid uninitialized starting
/// state, and `init_heap` below fills it in exactly once.
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Maps the fixed `HEAP_START..HEAP_START + HEAP_SIZE` virtual range to
/// freshly allocated physical frames and hands the whole range to
/// `ALLOCATOR`, so `alloc::alloc::alloc` (and therefore `Box`/`Vec`/etc.)
/// have somewhere to carve allocations from. Must run after `memory::init`
/// and `BootInfoFrameAllocator` are both live, since it needs both to create
/// the mapping.
pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + HEAP_SIZE as u64 - 1u64;
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page, heap_end_page)
    };

    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        // Safety: `page` falls inside the fixed heap range this function
        // owns exclusively, and `frame` was just freshly allocated, so
        // neither side of this mapping can already be in use elsewhere.
        unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
    }

    // Safety: the whole `HEAP_START..HEAP_START + HEAP_SIZE` range was just
    // mapped writable above, and this is the only place that ever calls
    // `init` on `ALLOCATOR`, so no other code can be using this memory yet.
    unsafe {
        ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::memory::{FRAME_ALLOCATOR, MAPPER};
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    #[test_case]
    fn test_boxed_value_round_trips() {
        let heap_value = Box::new(41);
        assert_eq!(*heap_value, 41);
    }

    #[test_case]
    fn test_many_small_allocations_reuse_the_heap() {
        // Exceeds HEAP_SIZE many times over if the allocator never reclaims
        // freed blocks, so this only passes if deallocation actually frees
        // space for reuse rather than just bump-allocating forever.
        for i in 0..1000 {
            let x = Box::new(i);
            assert_eq!(*x, i);
        }
    }

    #[test_case]
    fn test_vec_growth() {
        let mut vec = Vec::new();
        for i in 0..500 {
            vec.push(i);
        }
        assert_eq!(vec.iter().sum::<u64>(), (0..500).sum());
    }

    // Sanity check that the globals this module's `init_heap` depends on
    // really are the ones `kernel::init()` set up, not just that the heap
    // works in general.
    #[test_case]
    fn test_globals_used_by_init_heap_are_live() {
        MAPPER.get().expect("MAPPER not initialized").lock();
        FRAME_ALLOCATOR
            .get()
            .expect("FRAME_ALLOCATOR not initialized")
            .lock();
    }

    // Each cycle allocates then immediately drops, so at most one block is
    // ever live at a time - this is stressing sustained churn (allocate,
    // free, reuse) rather than peak heap usage, which `HEAP_SIZE` (100 KiB)
    // couldn't survive if every allocation below stayed alive at once.
    #[test_case]
    fn test_alloc_free_stress_cycles() {
        const CYCLES: usize = 10_000;
        const SIZES: [usize; 5] = [8, 64, 256, 1024, 4096];

        for i in 0..CYCLES {
            let size = SIZES[i % SIZES.len()];
            let pattern = (i % 256) as u8;
            let mut block: Vec<u8> = Vec::with_capacity(size);
            block.resize(size, pattern);
            assert_eq!(block.len(), size);
            assert!(block.iter().all(|&b| b == pattern));
            // `block` drops here, freeing back to the allocator before the
            // next cycle allocates again.
        }
    }

    // Uses `alloc::alloc::alloc`/`dealloc` directly rather than `Box`, since
    // `Box` would trivially always satisfy alignment via the type system -
    // this exercises `LockedHeap`'s handling of an arbitrary *requested*
    // alignment instead.
    #[test_case]
    fn test_alignment_respected_for_various_alignments() {
        use alloc::alloc::{alloc, dealloc};
        use core::alloc::Layout;

        for &align in &[1usize, 2, 4, 8, 16] {
            let layout = Layout::from_size_align(64, align).expect("valid layout");
            let ptr = unsafe { alloc(layout) };
            assert!(!ptr.is_null(), "allocation failed for alignment {}", align);
            assert_eq!(
                ptr as usize % align,
                0,
                "pointer {:p} not aligned to {}",
                ptr,
                align
            );
            unsafe { dealloc(ptr, layout) };
        }
    }

    // Scatters frees through the free list (rather than freeing everything
    // in one shot) by keeping roughly a third of ~200 varied-size blocks
    // alive at a time, then confirms a single large allocation still
    // succeeds afterward - i.e. the churn above didn't leak usable space the
    // allocator should have reclaimed.
    #[test_case]
    fn test_fragmentation_does_not_leak_usable_space() {
        const SIZES: [usize; 4] = [16, 128, 512, 32];

        let mut kept: Vec<Vec<u8>> = Vec::new();
        for i in 0..200 {
            let size = SIZES[i % SIZES.len()];
            let block = alloc::vec![i as u8; size];
            if i % 3 == 0 {
                kept.push(block);
            }
            // else: `block` drops immediately here.
        }
        drop(kept);

        let large = alloc::vec![0xAAu8; 40 * 1024]; // 40 KiB, well under HEAP_SIZE (100 KiB)
        assert_eq!(large.len(), 40 * 1024);
        assert!(large.iter().all(|&b| b == 0xAA));
    }
}
