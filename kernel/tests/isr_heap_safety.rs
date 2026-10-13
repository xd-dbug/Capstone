#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use bootloader_api::BootInfo;
use core::panic::PanicInfo;
use kernel::interrupts::{in_interrupt, IsrGuard};
use kernel::{exit_qemu, serial_print, serial_println, QemuExitCode};
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

lazy_static! {
    /// Test-local IDT whose breakpoint handler behaves like a buggy ISR: it
    /// declares interrupt context (as every real handler must) and then
    /// allocates, which `GuardedHeap` must turn into a panic.
    static ref TEST_IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(allocating_breakpoint_handler);
        idt
    };
}

extern "x86-interrupt" fn allocating_breakpoint_handler(_stack_frame: InterruptStackFrame) {
    let _isr = IsrGuard::enter();
    // `black_box` so the optimizer can't elide the allocation.
    let b = core::hint::black_box(Box::new(1u64));
    // Reached only if detection failed.
    serial_println!("[failed: ISR allocation went undetected: {}]", b);
    exit_qemu(QemuExitCode::Failed);
    loop {}
}

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

    // Mainline allocation must be unaffected by the guard (and by masking
    // interrupts around the heap lock): alloc, grow, free, outside an ISR.
    serial_print!("isr_heap_safety::normal_allocation_still_works...\t");
    assert!(!in_interrupt());
    let mut v: Vec<u64> = Vec::new();
    for i in 0..256 {
        v.push(i);
    }
    assert_eq!(v.iter().sum::<u64>(), (0..256).sum());
    drop(v);
    // `init` left interrupts on; the heap path must restore that state.
    assert!(x86_64::instructions::interrupts::are_enabled());
    serial_println!("[ok]");

    // The test IDT has no timer/keyboard entries, so mask interrupts before
    // swapping it in.
    x86_64::instructions::interrupts::disable();
    TEST_IDT.load();
    serial_print!("isr_heap_safety::allocation_in_isr_is_detected...\t");
    x86_64::instructions::interrupts::int3();

    serial_println!("[failed: int3 returned]");
    exit_qemu(QemuExitCode::Failed);
    loop {}
}

bootloader_api::entry_point!(test_kernel_main, config = &kernel::BOOTLOADER_CONFIG);

/// The expected panic must be the interrupt-context one, not any other
/// panic (an unrelated assert failure above must not pass this test).
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let expected = info
        .message()
        .as_str()
        .is_some_and(|m| m.contains("interrupt context"));
    if expected && in_interrupt() {
        serial_println!("[ok]");
        exit_qemu(QemuExitCode::Success);
    } else {
        serial_println!("[failed]\n");
        serial_println!("Error: {}\n", info);
        exit_qemu(QemuExitCode::Failed);
    }
    loop {}
}
