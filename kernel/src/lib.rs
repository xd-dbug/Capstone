#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![feature(abi_x86_interrupt)]
#![reexport_test_harness_main = "test_main"]

#[cfg(test)]
use bootloader_api::BootInfo;
use core::panic::PanicInfo;

pub mod framebuffer;
pub mod interrupts;
pub mod serial;
pub mod gdt;


/// Anything that can report itself as a named, pass/fail test over serial.
pub trait Testable {
    fn run(&self) -> ();
}

/// Brings up CPU-level state needed before anything else can run safely:
/// segment/TSS descriptors first, then the interrupt handlers that depend on them.
pub fn init() {
    gdt::init();
    interrupts::init_idt();
}

// Blanket impl: any zero-arg fn (i.e. every `#[test_case]`) is Testable for free.
impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

/// Custom `#[test_runner]`: runs every collected test, then shuts QEMU down
/// with a success exit code (there's no OS underneath to return control to).
pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

/// Shared `#[panic_handler]` for test builds: reports the failure over serial
/// and exits QEMU with a failure code so `cargo test` sees a non-zero result.
pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    loop {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// Writes to QEMU's `isa-debug-exit` I/O port to terminate the VM with a
/// specific exit code, standing in for a real "shutdown" syscall in tests.
pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

/// Entry point for `cargo test` against this library itself.
#[cfg(test)]
fn test_kernel_main(boot_info: &'static mut BootInfo) -> ! {
    if let Some(framebuffer) = boot_info.framebuffer.as_mut() {
        framebuffer::init(framebuffer);
    }
    init();
    test_main();
    loop {}
}

#[cfg(test)]
bootloader_api::entry_point!(test_kernel_main);

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}