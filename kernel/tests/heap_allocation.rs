#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::BootInfo;
use core::panic::PanicInfo;
use kernel::BOOTLOADER_CONFIG;

/// Integration test entry point: runs a full `kernel::init()` (heap
/// included) and exercises `Box`/`Vec`/`String` end-to-end through a real
/// boot, distinct from `allocator.rs`'s in-crate unit tests, which share the
/// `lib.rs` unit-test binary's own boot rather than getting a dedicated one.
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

#[test_case]
fn test_boxed_value() {
    use alloc::boxed::Box;

    let value = Box::new(99);
    assert_eq!(*value, 99);
}

#[test_case]
fn test_vec_of_values() {
    use alloc::vec::Vec;

    let mut v = Vec::new();
    for i in 0..100 {
        v.push(i);
    }
    assert_eq!(v.iter().sum::<u32>(), (0..100).sum());
}

#[test_case]
fn test_heap_allocated_string() {
    use alloc::string::String;

    let mut s = String::from("seal_os");
    s.push_str(" heap");
    assert_eq!(s, "seal_os heap");
}
