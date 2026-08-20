use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::registers::control::Cr2;
use crate::println;
use crate::gdt;
use pic8259::ChainedPics;
use spin::Mutex;
use crate::print;

use lazy_static::lazy_static;

// Built once, lazily, on first access; `static` requires a `const` initializer
// but table construction (method calls) isn't const, hence lazy_static.
lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        unsafe {
            // Double faults run on their own dedicated stack (set up in gdt.rs) so a
            // kernel stack overflow, which is itself a double fault, doesn't triple fault.
            idt.double_fault.set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt[InterruptIndex::Timer.as_u8()]
            .set_handler_fn(timer_interrupt_handler);

        idt
    };
}

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

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
/// Page faults are unrecoverable here (no demand paging/heap growth yet), so
/// this reports the faulting address/cause and halts rather than trying to
/// resume execution. Must loop rather than `hlt()` once: with the timer IRQ
/// unmasked, a single `hlt()` just sleeps until the next tick wakes it, and
/// returning re-executes (and re-faults on) the same instruction forever.
extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    println!("EXCEPTION: PAGE FAULT");
    println!("Accessed Address: {:?}", Cr2::read());
    println!("Error Code: {:?}", error_code);
    println!("{:#?}", stack_frame);
    loop {
        x86_64::instructions::hlt();
    }
}

#[test_case]
fn test_breakpoint_exception() {
    // invoke a breakpoint exception
    x86_64::instructions::interrupts::int3();
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

extern "x86-interrupt" fn timer_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    print!(".");

    // Without this, the PIC's in-service bit for IRQ0 never clears, so it
    // never delivers another timer interrupt after this one.
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}
