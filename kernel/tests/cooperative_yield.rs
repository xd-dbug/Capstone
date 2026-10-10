#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use bootloader_api::BootInfo;
use core::panic::PanicInfo;
use kernel::BOOTLOADER_CONFIG;
use kernel::scheduler::{ready_count, spawn, yield_now};
use spin::Mutex;

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

/// Shared order log. The lock is only held for the push, never across a
/// yield, or the next thread would spin on it forever.
static LOG: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn log(tag: u8) {
    LOG.lock().push(tag);
}

fn take_log() -> Vec<u8> {
    core::mem::take(&mut *LOG.lock())
}

/// Boot task keeps yielding until it is the only task left.
fn run_until_done() {
    while ready_count() > 0 {
        yield_now();
    }
}

extern "C" fn thread_a() {
    for _ in 0..3 {
        log(b'A');
        yield_now();
    }
}

extern "C" fn thread_b() {
    for _ in 0..3 {
        log(b'B');
        yield_now();
    }
}

/// Logs once and returns, exercising the `task_returned` -> `Finished` path.
extern "C" fn thread_early() {
    log(b'C');
}

#[test_case]
fn test_two_threads_alternate() {
    spawn(thread_a);
    spawn(thread_b);
    run_until_done();
    // Reaching this line also proves control returned to the boot task.
    assert_eq!(take_log(), vec![b'A', b'B', b'A', b'B', b'A', b'B']);
    assert_eq!(ready_count(), 0);
}

#[test_case]
fn test_early_return_while_other_continues() {
    spawn(thread_early);
    spawn(thread_b);
    run_until_done();
    assert_eq!(take_log(), vec![b'C', b'B', b'B', b'B']);
}

#[test_case]
fn test_three_threads_round_robin() {
    spawn(thread_a);
    spawn(thread_b);
    spawn(thread_early);
    run_until_done();
    assert_eq!(take_log(), vec![b'A', b'B', b'C', b'A', b'B', b'A', b'B']);
}

#[test_case]
fn test_yield_with_no_other_task_returns() {
    yield_now();
    assert_eq!(ready_count(), 0);
}
