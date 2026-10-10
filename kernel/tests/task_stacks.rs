#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::BootInfo;
use core::panic::PanicInfo;
use kernel::BOOTLOADER_CONFIG;

/// Integration test (not a lib unit test) because the lib binary's frame-allocator
/// tests scribble over already-handed-out frames, including heap ones, so the
/// heap is not trustworthy by the time `task` tests would run there.
fn test_kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let physical_memory_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("BOOTLOADER_CONFIG enables physical memory mapping");
    let memory_regions = &boot_info.memory_regions;

    if let Some(framebuffer) = boot_info.framebuffer.as_mut() {
        kernel::framebuffer::init(framebuffer);
    }
    kernel::init(physical_memory_offset, memory_regions);
    test_main();
    loop {}
}

bootloader_api::entry_point!(test_kernel_main, config = &BOOTLOADER_CONFIG);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::test_panic_handler(info)
}

use alloc::vec::Vec;
use kernel::task::{STACK_SIZE, Task};

#[test_case]
fn test_stacks_distinct_aligned_and_in_bounds() {
    // 3 matches W2's minimum concurrent task count.
    let tasks: Vec<Task> = (0..3).map(|_| Task::with_stack()).collect();
    for t in &tasks {
        let top = t.stack_top().unwrap().as_u64();
        let bottom = t.stack_bottom().unwrap().as_u64();
        assert_eq!(top % 16, 0);
        assert!(top > bottom && top <= bottom + STACK_SIZE as u64);
        assert!(bottom + (STACK_SIZE as u64) - top < 16);
        assert!(t.check_canary());
    }
    for (i, a) in tasks.iter().enumerate() {
        for b in &tasks[i + 1..] {
            assert_ne!(a.id(), b.id());
            let a0 = a.stack_bottom().unwrap().as_u64();
            let b0 = b.stack_bottom().unwrap().as_u64();
            assert!(a0 + STACK_SIZE as u64 <= b0 || b0 + STACK_SIZE as u64 <= a0);
        }
    }
}

#[test_case]
fn test_boot_task_has_no_stack() {
    let boot = Task::boot();
    assert!(boot.stack_top().is_none());
    assert!(boot.stack_bottom().is_none());
    assert!(boot.check_canary());
}

#[test_case]
fn test_stacks_freed_and_reallocated() {
    for _ in 0..50 {
        let t = Task::with_stack();
        assert!(t.check_canary());
    }
}
