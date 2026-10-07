#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use bootloader_api::BootInfo;
use core::panic::PanicInfo;
use kernel::{memory::MAPPER, serial_println, BOOTLOADER_CONFIG, KERNEL_HALF_START};

/// Integration test entry point: full `kernel::init()` so the heap and mapper
/// exist, then checks that everything the kernel owns lives in the upper half.
fn test_kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let physical_memory_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("BOOTLOADER_CONFIG enables physical memory mapping");
    let memory_regions = &boot_info.memory_regions;

    if let Some(framebuffer) = boot_info.framebuffer.as_mut() {
        // The framebuffer's backing buffer is a bootloader dynamic mapping.
        let addr = framebuffer.buffer().as_ptr() as u64;
        assert!(addr >= KERNEL_HALF_START, "framebuffer at {:#x}", addr);
        kernel::framebuffer::init(framebuffer);
    }
    assert!(
        memory_regions.as_ptr() as u64 >= KERNEL_HALF_START,
        "boot info memory map in lower half"
    );
    kernel::init(physical_memory_offset, memory_regions);
    test_main();
    loop {}
}

bootloader_api::entry_point!(test_kernel_main, config = &BOOTLOADER_CONFIG);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::test_panic_handler(info)
}

fn code_marker() {}

#[test_case]
fn test_kernel_code_and_stack_in_upper_half() {
    let local = 0u8;
    assert!(&local as *const u8 as u64 >= KERNEL_HALF_START);
    // Linked at the top 2 GiB so the `kernel` code model is valid.
    assert!(code_marker as usize as u64 >= 0xffff_ffff_8000_0000);
}

#[test_case]
fn test_heap_and_phys_offset_in_upper_half() {
    let boxed = Box::new(7u64);
    assert!(&*boxed as *const u64 as u64 >= KERNEL_HALF_START);
    let mapper = MAPPER.get().expect("MAPPER not initialized").lock();
    assert!(mapper.phys_offset().as_u64() >= KERNEL_HALF_START);
}

/// Reports (rather than asserts) what the bootloader leaves in the lower
/// half: its context-switch trampoline and GDT are identity-mapped in the
/// kernel's address space. The only assertions are that the kernel's own
/// entries (256 phys map, 384 heap, 511 image) are present.
#[test_case]
fn test_lower_half_pml4_entries() {
    let mapper = MAPPER.get().expect("MAPPER not initialized").lock();
    let p4 = mapper.level_4_table();
    let mut used = 0;
    for (i, entry) in p4.iter().enumerate().take(256) {
        if !entry.is_unused() {
            used += 1;
            serial_println!("lower-half PML4[{}] in use: {:?}", i, entry.flags());
        }
    }
    serial_println!("lower-half PML4 entries in use: {}", used);
    for (i, entry) in p4.iter().enumerate().skip(256) {
        if !entry.is_unused() {
            serial_println!("upper-half PML4[{}] in use", i);
        }
    }
    // Kernel mappings must be in the upper half only.
    assert!(!p4[256].is_unused(), "phys-memory map missing");
    assert!(!p4[511].is_unused(), "kernel image missing");
    assert!(!p4[384].is_unused(), "heap missing");
}
