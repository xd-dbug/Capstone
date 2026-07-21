#![no_std]
#![no_main]

use bootloader_api::BootInfo;
use core::panic::PanicInfo;
use kernel::{exit_qemu, serial_print, serial_println, QemuExitCode};

/// Integration test entry point: expects `should_fail` to panic. If it
/// somehow returns instead, that's the actual failure, hence exit code Failed here.
fn test_kernel_main(_boot_info: &'static mut BootInfo) -> ! {
    should_fail();
    serial_println!("[test did not panic]");
    exit_qemu(QemuExitCode::Failed);
    loop {}
}

bootloader_api::entry_point!(test_kernel_main);

/// Asserts something false on purpose so the panic handler below can verify
/// that panicking code is detected correctly (the inverse of a normal test).
fn should_fail() {
    serial_print!("should_panic::should_fail...\t");
    assert_eq!(0, 1);
}

// Panicking is the expected, successful outcome for this test, so this
// handler reports success rather than delegating to kernel::test_panic_handler.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    loop {}
}