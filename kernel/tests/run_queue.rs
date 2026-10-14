#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader_api::BootInfo;
use core::panic::PanicInfo;
use kernel::BOOTLOADER_CONFIG;
use kernel::scheduler::{MAX_TASKS, SpawnError, ready_count, spawn, yield_now};
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

/// Shared order log. The lock is only held for the push, never across a
/// yield, or the next thread would spin on it forever.
static LOG: Mutex<Vec<u8>> = Mutex::new(Vec::new());

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

/// `extern "C" fn()` can't capture, so one fn per tag.
macro_rules! logger {
    ($name:ident, $tag:expr, $rounds:expr) => {
        extern "C" fn $name() {
            for _ in 0..$rounds {
                log($tag);
                yield_now();
            }
        }
    };
}
logger!(t1, b'1', 3);
logger!(t2, b'2', 3);
logger!(t3, b'3', 3);
logger!(t4, b'4', 3);

extern "C" fn noop() {}

#[test_case]
fn test_round_robin_order_with_four_tasks() {
    for f in [t1, t2, t3, t4] {
        spawn(f).unwrap();
    }
    run_until_done();
    assert_eq!(take_log(), b"123412341234".to_vec());
}

#[test_case]
fn test_spawn_past_cap_errors_then_recovers() {
    // The boot task occupies one of the MAX_TASKS slots.
    for _ in 0..MAX_TASKS - 1 {
        spawn(noop).unwrap();
    }
    assert_eq!(ready_count(), MAX_TASKS - 1);
    assert_eq!(spawn(noop), Err(SpawnError::TooManyTasks));
    assert_eq!(spawn(noop), Err(SpawnError::TooManyTasks));
    assert_eq!(ready_count(), MAX_TASKS - 1);

    run_until_done();
    // Capacity comes back once the tasks finish and are reaped.
    spawn(t1).unwrap();
    run_until_done();
    assert_eq!(take_log(), b"111".to_vec());
}

#[test_case]
fn test_spawn_out_of_heap_errors() {
    // Eat the heap in 1 KiB pieces until none is left, so a 10 KiB stack
    // can't fit. The outer Vec is sized first so pushing never allocates.
    let mut hog: Vec<Vec<u8>> = Vec::with_capacity(128);
    while hog.len() < 128 {
        let mut chunk: Vec<u8> = Vec::new();
        if chunk.try_reserve_exact(1024).is_err() {
            break;
        }
        hog.push(chunk);
    }
    assert_eq!(spawn(noop), Err(SpawnError::OutOfMemory));
    assert_eq!(ready_count(), 0);
    drop(hog);
    spawn(noop).unwrap();
    run_until_done();
}

#[test_case]
fn test_yield_with_no_other_task_returns() {
    yield_now();
    assert_eq!(ready_count(), 0);
}
