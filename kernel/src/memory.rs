use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use conquer_once::spin::OnceCell;
use spin::Mutex;
use x86_64::structures::paging::{FrameAllocator, PageSize, PhysFrame, Size4KiB};
use x86_64::PhysAddr;

/// Global handle to the frame allocator, set up once by `kernel::init()`.
pub static FRAME_ALLOCATOR: OnceCell<Mutex<BootInfoFrameAllocator>> = OnceCell::uninit();

/// Bump allocator over the bootloader's usable physical memory regions: each
/// `allocate_frame` call hands out the next 4 KiB frame and advances a
/// cursor. Frames are never reclaimed yet — that's the free-list layered on
/// top in a later pass.
pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryRegions,
    next: usize,
}

impl BootInfoFrameAllocator {
    /// # Safety
    ///
    /// `memory_map` must be accurate: every region marked `Usable` must
    /// really be unused RAM (not overlapping the kernel, page tables, boot
    /// info, or a physical MMIO range), or a later `allocate_frame` could
    /// hand out a frame something else is already using.
    pub unsafe fn init(memory_map: &'static MemoryRegions) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
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
}

// # Safety
// `usable_frames()` only ever yields frames carved out of `Usable`
// regions, and the bump cursor (`next`) only moves forward, so no frame is
// ever handed out twice or pulled from memory something else already owns.
unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}

// Each test builds its own `BootInfoFrameAllocator` from `FRAME_ALLOCATOR`'s
// already-initialized `memory_map`, instead of allocating through the shared
// global directly — otherwise the two tests would fight over one bump
// cursor and their results would depend on run order.

#[test_case]
fn test_allocate_frames_distinct_and_aligned() {
    const N: usize = 16;
    let memory_map = FRAME_ALLOCATOR
        .get()
        .expect("FRAME_ALLOCATOR not initialized")
        .lock()
        .memory_map;
    let mut allocator = unsafe { BootInfoFrameAllocator::init(memory_map) };
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
    let memory_map = FRAME_ALLOCATOR
        .get()
        .expect("FRAME_ALLOCATOR not initialized")
        .lock()
        .memory_map;
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

    let mut allocator = unsafe { BootInfoFrameAllocator::init(memory_map) };
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
