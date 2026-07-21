#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use bootloader_api::BootInfo;
use kernel::{exit_qemu, QemuExitCode, serial_println};
use x86_64::structures::idt::InterruptStackFrame;
use core::panic::PanicInfo;
use kernel::serial_print;
use lazy_static::lazy_static;
use x86_64::structures::idt::InterruptDescriptorTable;

/// Integration test entry point: verifies a kernel stack overflow is caught
/// as a double fault (via the IST-backed handler) rather than triple-faulting the VM.
fn test_kernel_main(_boot_info: &'static mut BootInfo) -> ! {
    serial_print!("stack_overflow::stack_overflow...\t");

    kernel::gdt::init();
    init_test_idt();

    // trigger a stack overflow
    stack_overflow();

    panic!("Execution continued after stack overflow");
}

bootloader_api::entry_point!(test_kernel_main);

/// Recurses until the stack guard page is hit, deliberately triggering the fault under test.
#[allow(unconditional_recursion)]
fn stack_overflow() {
    stack_overflow(); // for each recursion, the return address is pushed
    volatile::Volatile::new(0).read(); // prevent tail recursion optimizations
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::test_panic_handler(info)
}

lazy_static! {
    static ref TEST_IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        unsafe {
            idt.double_fault
                .set_handler_fn(test_double_fault_handler)
                .set_stack_index(kernel::gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt
    };
}

pub fn init_test_idt() {
    TEST_IDT.load();
}

/// Reaching this handler is the test *passing*: the double fault was caught
/// safely, so it reports success and exits instead of treating it as a crash.
extern "x86-interrupt" fn test_double_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    loop {}
}