use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
use crate::println;
use crate::gdt;

use lazy_static::lazy_static;

// Built once, lazily, on first access; `static` requires a `const` initializer
// but table construction (method calls) isn't const, hence lazy_static.
lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        unsafe {
            // Double faults run on their own dedicated stack (set up in gdt.rs) so a
            // kernel stack overflow, which is itself a double fault, doesn't triple fault.
            idt.double_fault.set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX); // new
        }

        idt
    };
}

/// Installs the IDT so the CPU starts routing exceptions/interrupts through it.
pub fn init_idt() {
    IDT.load();
}

/// Handles `int3`/breakpoint exceptions by logging and returning (execution resumes).
extern "x86-interrupt" fn breakpoint_handler(
    stack_frame: InterruptStackFrame)
{
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

/// Double faults are unrecoverable here, so this just reports and panics
/// instead of returning (the `!` return type is required for this vector).
extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame, _error_code: u64) -> !
{
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}
#[test_case]
fn test_breakpoint_exception() {
    // invoke a breakpoint exception
    x86_64::instructions::interrupts::int3();
}