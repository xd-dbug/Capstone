use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use conquer_once::spin::OnceCell;
use spin::Mutex;
use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, PageSize, PhysFrame, Size4KiB};
use x86_64::PhysAddr;
use x86_64::VirtAddr;

/// Global handle to the frame allocator, set up once by `kernel::init()`.
pub static FRAME_ALLOCATOR: OnceCell<Mutex<BootInfoFrameAllocator>> = OnceCell::uninit();

/// Marks the tail of the free list: no valid physical frame's start address
/// (52-bit max in practice) can ever equal this, so it's safe to use as a
/// sentinel stored alongside real addresses in freed frames' own memory.
const FREE_LIST_END: u64 = u64::MAX;

/// Bump allocator over the bootloader's usable physical memory regions, with
/// a singly-linked free list layered on top so `deallocate_frame` can
/// actually reclaim: each freed frame's own backing memory (reached via the
/// physical-memory offset mapping) stores the address of the next free
/// frame, since no heap allocator exists yet to back a real linked-list
/// type. `allocate_frame` drains the free list before advancing the bump
/// cursor, so reclaimed frames are reused ahead of untouched memory.
pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryRegions,
    physical_memory_offset: VirtAddr,
    next: usize,
    free_list_head: Option<PhysFrame>,
}

impl BootInfoFrameAllocator {
    /// # Safety
    ///
    /// `memory_map` must be accurate: every region marked `Usable` must
    /// really be unused RAM (not overlapping the kernel, page tables, boot
    /// info, or a physical MMIO range), or a later `allocate_frame` could
    /// hand out a frame something else is already using.
    pub unsafe fn init(memory_map: &'static MemoryRegions, physical_memory_offset: VirtAddr) -> Self {
        // `frame_ptr` does an aligned `u64` read/write through this offset,
        // which is only sound if the offset itself is 8-byte aligned (frame
        // addresses are always 4 KiB-aligned already). True for any mapping
        // the `bootloader` crate sets up in practice, but not enforced by
        // its API, so check it here instead of failing silently on UB.
        debug_assert!(physical_memory_offset.is_aligned(8u64));
        BootInfoFrameAllocator {
            memory_map,
            physical_memory_offset,
            next: 0,
            free_list_head: None,
        }
    }

    /// Every 4 KiB-aligned frame inside the map's `Usable` regions, in
    /// order. Region start addresses are rounded up to the next frame
    /// boundary first — the bootloader's regions are usually already
    /// page-aligned, but nothing guarantees it, and stepping from an
    /// unaligned start would hand out frames that straddle two frame
    /// boundaries.
    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        self.memory_map
            .iter()
            .filter(|region| region.kind == MemoryRegionKind::Usable)
            .flat_map(|region| {
                let aligned_start = region.start.next_multiple_of(Size4KiB::SIZE);
                (aligned_start..region.end).step_by(Size4KiB::SIZE as usize)
            })
            .map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }

    /// The offset-mapped virtual pointer to the start of `frame`'s backing
    /// physical memory. Physical memory isn't identity-mapped, so this is
    /// the only way to read or write a frame's bytes directly (as opposed
    /// to through a page table mapping something else set up separately).
    ///
    /// # Safety
    ///
    /// The caller must not use the returned pointer while `frame` is mapped
    /// and in use elsewhere — writing through it would corrupt whatever
    /// that other mapping thinks is there. Only sound to call on a frame
    /// this allocator itself is about to hand out or has just reclaimed.
    unsafe fn frame_ptr(&self, frame: PhysFrame) -> *mut u64 {
        (self.physical_memory_offset + frame.start_address().as_u64()).as_mut_ptr()
    }
}

// # Safety
// `usable_frames()` only ever yields frames carved out of `Usable`
// regions, and the bump cursor (`next`) only moves forward, so no frame is
// ever handed out twice or pulled from memory something else already owns.
// The free list can only contain frames this same allocator previously
// handed out and had returned via `deallocate_frame`, so popping it is
// equally sound.
unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        if let Some(frame) = self.free_list_head {
            // Safety: `frame` was pushed by `deallocate_frame`, which wrote
            // a valid next-pointer (or `FREE_LIST_END`) into its backing
            // memory before linking it in, and nothing else can be using
            // that memory while it sits on the free list.
            let next = unsafe { self.frame_ptr(frame).read() };
            self.free_list_head = (next != FREE_LIST_END)
                .then(|| PhysFrame::containing_address(PhysAddr::new(next)));
            return Some(frame);
        }

        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}

impl FrameDeallocator<Size4KiB> for BootInfoFrameAllocator {
    /// # Safety
    ///
    /// `frame` must not be mapped or otherwise in use anywhere else, and
    /// must not already be on this allocator's free list — freeing the same
    /// frame twice links it to itself, and every later `allocate_frame`
    /// would then hand out that one physical frame to multiple callers at
    /// once. A debug-mode check catches the immediate case (freeing the
    /// current free-list head again); a frame double-freed after other
    /// frees have moved the head elsewhere is not detected.
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame) {
        debug_assert_ne!(
            self.free_list_head,
            Some(frame),
            "double-free: frame {:?} is already the free-list head",
            frame
        );
        let next = self
            .free_list_head
            .map_or(FREE_LIST_END, |f| f.start_address().as_u64());
        // Safety: `frame` is caller-guaranteed unused, and this allocator's
        // own free-list traversal is the only thing that will read this
        // write back out.
        unsafe { self.frame_ptr(frame).write(next) };
        self.free_list_head = Some(frame);
    }
}

// Each test builds its own `BootInfoFrameAllocator` from `FRAME_ALLOCATOR`'s
// already-initialized `memory_map`, instead of allocating through the shared
// global directly — otherwise the two tests would fight over one bump
// cursor and their results would depend on run order.

#[test_case]
fn test_allocate_frames_distinct_and_aligned() {
    const N: usize = 16;
    let (memory_map, physical_memory_offset) = {
        let guard = FRAME_ALLOCATOR
            .get()
            .expect("FRAME_ALLOCATOR not initialized")
            .lock();
        (guard.memory_map, guard.physical_memory_offset)
    };
    let mut allocator = unsafe { BootInfoFrameAllocator::init(memory_map, physical_memory_offset) };
    let frames: [PhysFrame; N] =
        core::array::from_fn(|_| allocator.allocate_frame().expect("ran out of usable frames"));

    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame.start_address().as_u64() % Size4KiB::SIZE, 0);
        assert!(
            !frames[..i].contains(frame),
            "frame {:?} was handed out twice",
            frame
        );
    }
}

#[test_case]
fn test_allocate_across_region_boundary() {
    let (memory_map, physical_memory_offset) = {
        let guard = FRAME_ALLOCATOR
            .get()
            .expect("FRAME_ALLOCATOR not initialized")
            .lock();
        (guard.memory_map, guard.physical_memory_offset)
    };
    let mut usable_regions = memory_map
        .iter()
        .filter(|region| region.kind == MemoryRegionKind::Usable);
    let first_region = usable_regions.next().expect("no usable region in memory map");
    let second_region = usable_regions
        .next()
        .expect("need at least two usable regions to exercise a boundary crossing");

    let first_region_frame_count = {
        let aligned_start = first_region.start.next_multiple_of(Size4KiB::SIZE);
        (first_region.end - aligned_start) / Size4KiB::SIZE
    };

    let mut allocator = unsafe { BootInfoFrameAllocator::init(memory_map, physical_memory_offset) };
    let mut crossed_into_second_region = false;
    for _ in 0..=first_region_frame_count {
        let addr = allocator
            .allocate_frame()
            .expect("ran out of usable frames before crossing the region boundary")
            .start_address()
            .as_u64();
        assert!(
            (first_region.start..first_region.end).contains(&addr)
                || (second_region.start..second_region.end).contains(&addr),
            "frame {:#x} came from neither the first nor second usable region",
            addr
        );
        crossed_into_second_region |= (second_region.start..second_region.end).contains(&addr);
    }
    assert!(
        crossed_into_second_region,
        "expected the bump cursor to cross into the second usable region"
    );
}

#[test_case]
fn test_deallocate_frame_is_reused_before_bump_cursor_advances() {
    let (memory_map, physical_memory_offset) = {
        let guard = FRAME_ALLOCATOR
            .get()
            .expect("FRAME_ALLOCATOR not initialized")
            .lock();
        (guard.memory_map, guard.physical_memory_offset)
    };
    let mut allocator = unsafe { BootInfoFrameAllocator::init(memory_map, physical_memory_offset) };

    let first = allocator.allocate_frame().expect("ran out of usable frames");
    let second = allocator.allocate_frame().expect("ran out of usable frames");
    unsafe { allocator.deallocate_frame(first) };

    let reused = allocator.allocate_frame().expect("ran out of usable frames");
    assert_eq!(
        first, reused,
        "expected the freed frame to be handed back out before the bump cursor advanced further"
    );

    // With the free list now drained, the bump cursor should resume from
    // where it left off (a fresh third frame), not repeat `second`.
    let third = allocator.allocate_frame().expect("ran out of usable frames");
    assert_ne!(third, first);
    assert_ne!(third, second);
}
