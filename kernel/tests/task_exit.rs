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
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::BOOTLOADER_CONFIG;
use kernel::allocator::used_bytes;
use kernel::scheduler::{exit, finished_count, ready_count, spawn, yield_now};
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
    // These tests assert exact interleavings and counts that a timer tick
    // between a log push and a `yield_now` would reorder. Only one task
    // exists until the first `spawn`, so a tick before this line can't switch.
    kernel::scheduler::set_preemption(false);
    test_main();
    loop {}
}

bootloader_api::entry_point!(test_kernel_main, config = &BOOTLOADER_CONFIG);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::test_panic_handler(info)
}

static LOG: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static SEEN_FINISHED: AtomicUsize = AtomicUsize::new(usize::MAX);

fn log(tag: u8) {
    LOG.lock().push(tag);
}

fn take_log() -> Vec<u8> {
    core::mem::take(&mut *LOG.lock())
}

fn run_until_done() {
    while ready_count() > 0 {
        yield_now();
    }
}

extern "C" fn exits_explicitly() {
    log(b'E');
    exit();
}

extern "C" fn deep(n: u32) {
    if n == 0 {
        log(b'D');
        exit();
    }
    deep(n - 1);
    // Unreachable in practice; stops the compiler turning this into a loop.
    log(b'!');
}

extern "C" fn exits_from_nested_call() {
    deep(8);
    log(b'!');
}

extern "C" fn records_finished_count() {
    SEEN_FINISHED.store(finished_count(), Ordering::SeqCst);
}

extern "C" fn does_nothing() {}

#[test_case]
fn test_explicit_exit() {
    spawn(exits_explicitly).unwrap();
    run_until_done();
    // Code after `exit()` never ran, and control came back to the boot task.
    assert_eq!(take_log(), vec![b'E']);
}

#[test_case]
fn test_exit_from_nested_call() {
    spawn(exits_from_nested_call).unwrap();
    run_until_done();
    assert_eq!(take_log(), vec![b'D']);
}

#[test_case]
fn test_fresh_task_reaps_before_entry() {
    spawn(exits_explicitly).unwrap();
    spawn(records_finished_count).unwrap();
    run_until_done();
    take_log();
    // Without the trampoline reap the first task's stack would still be
    // pending when the second task starts.
    assert_eq!(SEEN_FINISHED.load(Ordering::SeqCst), 0);
    assert_eq!(finished_count(), 0);
}

#[test_case]
fn test_heap_returns_to_baseline() {
    // Warm-up so any one-time allocation on first use isn't counted.
    spawn(does_nothing).unwrap();
    run_until_done();
    let baseline = used_bytes();
    // 30 stacks of 10 KiB can't fit in the 100 KiB heap at once, so this also
    // fails outright if exited stacks leak.
    for i in 0..30 {
        if i % 2 == 0 {
            spawn(does_nothing).unwrap();
        } else {
            spawn(exits_from_nested_call).unwrap();
        }
        run_until_done();
    }
    take_log();
    assert_eq!(finished_count(), 0);
    // Exact byte count, so even a leaked `Box<Task>` (not just a 10 KiB
    // stack) shows up.
    assert_eq!(used_bytes(), baseline);
}
