#![no_std] // don't link the Rust standard library
#![no_main] // disable all Rust-level entry points

use bootloader_api::BootInfo;
use core::panic::PanicInfo;

/// This function is called on panic.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    if let Some(framebuffer) = boot_info.framebuffer.as_mut() {
        let info = framebuffer.info();
        let buffer = framebuffer.buffer_mut();
        for chunk in buffer.chunks_mut(info.bytes_per_pixel) {
            chunk[0] = 0x00;
            chunk[1] = 0xff;
            chunk[2] = 0x00;
        }
    }

    loop {}
}

bootloader_api::entry_point!(kernel_main);