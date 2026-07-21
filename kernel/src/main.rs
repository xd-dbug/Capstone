#![no_std] // don't link the Rust standard library
#![no_main] // disable all Rust-level entry points
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader_api::BootInfo;
use core::panic::PanicInfo;
use kernel::println;

/// Kernel entry point invoked by the bootloader once long mode is set up.
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    if let Some(framebuffer) = boot_info.framebuffer.as_mut() {
        kernel::framebuffer::init(framebuffer);
        println!("Hello World{}", "!");

        kernel::init(); // new

        // fn stack_overflow() {
        //     stack_overflow(); // for each recursion, the return address is pushed
        // }
        //
        // // trigger a stack overflow
        // stack_overflow();

        #[cfg(test)]
        test_main();

        println!("It did not crash!");
    }

    loop {}
}

bootloader_api::entry_point!(kernel_main);

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}

/// This function is called on panic.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("KERNEL PANIC: {}", info);
    loop {}
}

// In test builds, route panics through the shared test harness instead so a
// failing test reports over serial and exits QEMU rather than hanging.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::test_panic_handler(info)
}