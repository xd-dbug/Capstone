#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use bootloader_api::BootInfo;
use core::panic::PanicInfo;
use kernel::{exit_qemu, serial_print, serial_println, QemuExitCode};
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

/// Integration test entry point: verifies a fault against an unmapped address
/// is caught and reported (via a test-local IDT override) rather than
/// triple-faulting or hanging the VM the way the kernel's real,
/// non-exiting `page_fault_handler` would in a test context.
fn test_kernel_main(_boot_info: &'static mut BootInfo) -> ! {
    serial_print!("page_fault::page_fault...\t");

    kernel::gdt::init();
    init_test_idt();

    // Deliberately touch an address nothing has mapped, to trigger #PF.
    let ptr = 0xdeadbeaf000 as *mut u8;
    unsafe { ptr.write_volatile(42) };

    panic!("Execution continued after page fault");
}

bootloader_api::entry_point!(test_kernel_main);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::test_panic_handler(info)
}

lazy_static! {
    static ref TEST_IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.page_fault.set_handler_fn(test_page_fault_handler);
        idt
    };
}

pub fn init_test_idt() {
    TEST_IDT.load();
}

/// Reaching this handler is the test *passing*: the fault against an
/// unmapped address was caught safely instead of triple-faulting/hanging, so
/// it reports success and exits instead of trying to resume execution.
extern "x86-interrupt" fn test_page_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: PageFaultErrorCode,
) {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    loop {
        x86_64::instructions::hlt();
    }
}
