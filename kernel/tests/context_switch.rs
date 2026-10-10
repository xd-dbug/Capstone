#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use bootloader_api::BootInfo;
use core::arch::asm;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use kernel::BOOTLOADER_CONFIG;
use kernel::context::{init_stack, switch};
use x86_64::VirtAddr;

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

/// Where each side's `rsp` is parked while the other runs. Atomics (not
/// `static mut`) per project convention; `as_ptr` hands `switch` the raw
/// pointer it writes through. `AtomicU64` and `VirtAddr` are both a bare u64.
static BOOT_RSP: AtomicU64 = AtomicU64::new(0);
static TASK_RSP: AtomicU64 = AtomicU64::new(0);
static RUNS: AtomicU64 = AtomicU64::new(0);
/// Set by the task if its stack was misaligned or its registers were clobbered.
static TASK_BAD: AtomicU64 = AtomicU64::new(0);

/// Alignment of the heap block is what makes the top 16-aligned.
#[repr(align(16))]
// The field is never read; it exists only for its size and alignment.
#[allow(dead_code)]
struct TestStack([u8; 16 * 1024]);

/// Switches to the task and back; returns the callee-saved r12/r13 values the
/// boot side observes after the round trip.
fn round_trip(task_rsp: VirtAddr) -> (u64, u64) {
    let (r12, r13): (u64, u64);
    // Safety: `task_rsp` is a live frame from `init_stack` or from the task's
    // previous `switch`. r12/r13 are set to sentinels before the call and read
    // after it; `switch` must preserve them. Declared as in/outs so the
    // compiler reads back whatever is in the registers after the call.
    unsafe {
        asm!(
            "mov r12, 0x1212121212121212",
            "mov r13, 0x1313131313131313",
            "call {sw}",
            sw = sym switch,
            in("rdi") BOOT_RSP.as_ptr(),
            in("rsi") task_rsp.as_u64(),
            inout("r12") 0u64 => r12,
            inout("r13") 0u64 => r13,
            clobber_abi("C"),
        );
    }
    (r12, r13)
}

extern "C" fn task_entry() {
    loop {
        RUNS.fetch_add(1, Ordering::SeqCst);
        let rsp: u64;
        // Safety: reads rsp only.
        unsafe { asm!("mov {}, rsp", out(reg) rsp) };
        // Inside a function body rsp stays 16-aligned between calls.
        if rsp % 16 != 0 {
            TASK_BAD.fetch_or(1, Ordering::SeqCst);
        }
        // Clobber callee-saved registers; switch must keep them per-task, not leak them back.
        // Safety: they are declared clobbered.
        unsafe {
            asm!(
                "mov r12, 0xdead",
                "mov r13, 0xbeef",
                out("r12") _,
                out("r13") _,
            );
        }
        // Safety: BOOT_RSP holds a frame saved by the boot side's `switch`.
        unsafe {
            switch(
                TASK_RSP.as_ptr() as *mut VirtAddr,
                VirtAddr::new(BOOT_RSP.load(Ordering::SeqCst)),
            )
        };
    }
}

#[test_case]
fn test_switch_round_trips() {
    let stack = Box::new(TestStack([0; 16 * 1024]));
    let top = VirtAddr::from_ptr(&*stack as *const TestStack) + core::mem::size_of::<TestStack>() as u64;
    // Safety: top is 16-aligned (struct alignment/size) with the whole stack below it.
    let mut task_rsp = unsafe { init_stack(top, task_entry) };

    for i in 1..=5u64 {
        let (r12, r13) = round_trip(task_rsp);
        assert_eq!(RUNS.load(Ordering::SeqCst), i);
        assert_eq!(r12, 0x1212121212121212);
        assert_eq!(r13, 0x1313131313131313);
        assert_eq!(TASK_BAD.load(Ordering::SeqCst), 0);
        // Resume where the task left off next time, not at the entry.
        task_rsp = VirtAddr::new(TASK_RSP.load(Ordering::SeqCst));
    }
}
