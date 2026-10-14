use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::registers::control::Cr2;
use crate::println;
use crate::gdt;
use pic8259::ChainedPics;
use spin::Mutex;
use crate::print;
use pc_keyboard::{layouts, DecodedKey, HandleControl, PS2Keyboard, ScancodeSet1};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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
        idt[InterruptIndex::Keyboard.as_u8()]
            .set_handler_fn(keyboard_interrupt_handler);

        idt
    };
}

/// Remapped off the default 0x08/0x70 (which overlaps CPU exception vectors)
/// to one past the last CPU exception vector (31), so hardware IRQs and CPU
/// exceptions can never land on the same IDT entry.
pub const PIC_1_OFFSET: u8 = 32;
/// The slave PIC's 8 IRQ lines follow the master's contiguously.
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

/// Locked from both timer and keyboard ISRs (to send EOI) and from
/// `kernel::init` (to remap/unmask), so it's one shared `Mutex` rather than
/// per-handler state.
pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

// Single core, so this only has to order against code on the same core.
// Acquire on enter / Release on exit (free on x86) keeps the compiler from
// moving work guarded by "we are in an ISR" outside the guard's lifetime.
//
// The depth describes the *currently running* context, not the CPU: any
// code that switches tasks from inside an ISR must save it before `switch`
// and restore it after, so each task resumes with its own depth (and a fresh
// task starts at 0).
static ISR_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// True while any `IsrGuard` is alive, i.e. we are inside an interrupt or
/// exception handler. `allocator::GuardedHeap` panics on heap use when set.
pub fn in_interrupt() -> bool {
    ISR_DEPTH.load(Ordering::Acquire) != 0
}

/// RAII marker for "this code runs in interrupt context". Every handler
/// (including any scheduler code the timer ISR calls) must create one first
/// thing; it is a counter, not a flag, so nested handlers (e.g. a breakpoint
/// inside the timer ISR) unwind correctly. Drop it only after the handler's
/// last heap-free work, i.e. just let it fall out of scope at the end.
///
/// The guard lives on the interrupted task's stack, so dropping it early
/// before a task switch is wrong: it would decrement again when that task
/// resumes and its handler returns. Save/restore `ISR_DEPTH` around the
/// switch instead (see the comment on `ISR_DEPTH`).
#[must_use = "the guard marks interrupt context only while it is alive"]
pub struct IsrGuard(());

impl IsrGuard {
    pub fn enter() -> Self {
        ISR_DEPTH.fetch_add(1, Ordering::Acquire);
        IsrGuard(())
    }
}

impl Drop for IsrGuard {
    fn drop(&mut self) {
        ISR_DEPTH.fetch_sub(1, Ordering::Release);
    }
}

/// Installs the IDT so the CPU starts routing exceptions/interrupts through it.
pub fn init_idt() {
    IDT.load();
}

/// Handles `int3`/breakpoint exceptions by logging and returning (execution resumes).
extern "x86-interrupt" fn breakpoint_handler(
    stack_frame: InterruptStackFrame)
{
    let _isr = IsrGuard::enter();
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

/// Double faults are unrecoverable here, so this just reports and panics
/// instead of returning (the `!` return type is required for this vector).
extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame, _error_code: u64) -> !
{
    let _isr = IsrGuard::enter();
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
    let _isr = IsrGuard::enter();
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

/// IDT vectors for the hardware IRQs this kernel handles, derived from
/// `PIC_1_OFFSET` so they stay correct if the PIC remap offset ever moves.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard = InterruptIndex::Timer as u8 + 1,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

// Relaxed everywhere: this is just a liveness counter, not synchronizing
// access to anything else, so no ordering guarantees beyond atomicity are
// needed.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Number of timer interrupts handled since boot.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

extern "x86-interrupt" fn timer_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    let _isr = IsrGuard::enter();
    TICKS.fetch_add(1, Ordering::Relaxed);

    // Without this, the PIC's in-service bit for IRQ0 never clears, so it
    // never delivers another timer interrupt after this one.
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

// Built once, lazily: the keyboard's decode state (shift/ctrl modifiers,
// multi-byte scancode sequences in flight) has to persist across interrupts,
// so it can't just be a local in the handler.
lazy_static! {
    static ref KEYBOARD: Mutex<PS2Keyboard<layouts::Us104Key, ScancodeSet1>> = Mutex::new(
        PS2Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::Ignore)
    );
}

/// Reads the scancode the PS/2 controller left on port 0x60 and feeds it
/// through `pc_keyboard`'s stateful decoder, which may need several scancode
/// bytes (extended/multi-byte sequences) before it yields a `DecodedKey`.
extern "x86-interrupt" fn keyboard_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    use x86_64::instructions::port::Port;

    let _isr = IsrGuard::enter();
    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };

    let mut keyboard = KEYBOARD.lock();
    if let Ok(Some(key_event)) = keyboard.add_byte(scancode)
        && let Some(key) = keyboard.process_keyevent(key_event)
    {
        match key {
            DecodedKey::Unicode(character) => print!("{}", character),
            DecodedKey::RawKey(key) => print!("{:?}", key),
        }
    }

    // Without this, the PIC's in-service bit for IRQ1 never clears, so it
    // never delivers another keyboard interrupt after this one.
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

#[test_case]
fn test_timer_ticks_increase() {
    let start = ticks();
    // `hlt` sleeps the CPU until the next interrupt, so this wakes on every
    // timer tick instead of spinning at full speed while waiting.
    while ticks() == start {
        x86_64::instructions::hlt();
    }
    assert!(ticks() > start);
}

#[test_case]
fn test_isr_guard_nests_and_clears() {
    assert!(!in_interrupt());
    {
        let _outer = IsrGuard::enter();
        assert!(in_interrupt());
        {
            let _inner = IsrGuard::enter();
            assert!(in_interrupt());
        }
        assert!(in_interrupt());
    }
    assert!(!in_interrupt());
}
