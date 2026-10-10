use core::arch::naked_asm;
use x86_64::VirtAddr;

/// Number of callee-saved registers `switch` pushes (rbx, rbp, r12-r15).
const SAVED_REGS: usize = 6;

/// Cooperative context switch: saves the outgoing task's callee-saved
/// registers on its own stack, stores `rsp` into `*old_rsp`, loads `new_rsp`,
/// and restores the incoming task's registers from that stack.
///
/// Only the callee-saved registers need saving because `switch` is an
/// ordinary `extern "C"` call from the compiler's point of view: it already
/// assumes rax, rcx, rdx, rsi, rdi, r8-r11 (and flags) are clobbered across
/// any call. A preemptive switch from a timer IRQ (W2) interrupts code that
/// made no such call, so it would have to save everything, via the interrupt
/// frame.
///
/// Naked because a normal function gets a compiler-generated prologue and
/// epilogue that would touch `rsp` (and possibly callee-saved registers)
/// before our code runs, corrupting the frame layout this relies on.
///
/// Stack frame it leaves behind (and expects), low to high address:
/// `r15, r14, r13, r12, rbp, rbx, return address`.
///
/// # Safety
/// - `old_rsp` must be valid for a write of one `VirtAddr`.
/// - `new_rsp` must point at a frame laid out by a previous `switch` out of
///   that stack, or by `init_stack`, and that stack must still be alive and
///   not currently running.
/// - Must not be called from an interrupt handler: the IRQ's own frame and
///   the PIC end-of-interrupt state would be stranded on the old stack.
/// - Do not hold a spinlock across the call (the other task could spin on it
///   forever, as there is no preemption).
#[unsafe(naked)]
pub unsafe extern "C" fn switch(old_rsp: *mut VirtAddr, new_rsp: VirtAddr) {
    naked_asm!(
        // The `call` already pushed the return address; these six follow it.
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // rdi = old_rsp (1st arg): publish where this task's registers live.
        "mov [rdi], rsp",
        // rsi = new_rsp (2nd arg): from here on we run on the other stack.
        "mov rsp, rsi",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",
        // Resumes the incoming task after its own `switch` call, or, for a
        // fresh task, enters `task_trampoline`.
        "ret",
    )
}

/// First code a fresh task runs, reached by `switch`'s `ret`. `init_stack`
/// parks the entry function in the rbx slot, so the `pop rbx` before that
/// `ret` has loaded it here.
///
/// A trampoline (rather than putting `entry` directly in the return slot)
/// gives a defined place to land if the entry returns, instead of `ret`ing
/// into whatever garbage sits above the stack top. At this point
/// `rsp % 16 == 0`, so `call` leaves the entry with `rsp % 16 == 8`, exactly
/// as SysV requires at a function's first instruction.
#[unsafe(naked)]
unsafe extern "C" fn task_trampoline() -> ! {
    naked_asm!(
        "call rbx",
        "call {exited}",
        "ud2",
        exited = sym task_returned,
    )
}

/// Separate from the trampoline (not inline asm) so the "task is done" logic
/// stays ordinary Rust. The scheduler marks the task `Finished` and switches
/// away for good.
extern "C" fn task_returned() -> ! {
    crate::scheduler::exit_current()
}

/// Builds the initial frame for a new task and returns the `saved_rsp` to
/// pass to `switch` as `new_rsp`.
///
/// Layout, top-down from `stack_top` (each slot 8 bytes):
/// ```text
/// stack_top       <- 16-byte aligned; nothing is written at or above it
/// stack_top - 8     task_trampoline   (ret target)
/// stack_top - 16    entry             (rbx)
/// stack_top - 24    0                 (rbp)
/// stack_top - 32    0                 (r12)
/// stack_top - 40    0                 (r13)
/// stack_top - 48    0                 (r14)
/// stack_top - 56    0                 (r15)  <- returned saved_rsp
/// ```
/// After `switch` pops six registers and `ret`s, `rsp == stack_top`, which is
/// 16-aligned, and the trampoline's `call` makes it 8 mod 16 inside `entry`.
///
/// `entry` is a plain `extern "C" fn()` (not `-> !`) so callers may write
/// ordinary functions; returning is caught by `task_returned` instead of
/// being undefined. `extern "C"` because the trampoline calls it with the
/// SysV ABI, which the Rust ABI does not promise.
///
/// # Safety
/// `stack_top` must be 16-byte aligned, and the 56 bytes below it must be
/// writable memory owned by the caller that outlives the task.
pub unsafe fn init_stack(stack_top: VirtAddr, entry: extern "C" fn()) -> VirtAddr {
    debug_assert!(stack_top.is_aligned(16u64));
    let frame = (stack_top.as_u64() - ((SAVED_REGS + 1) * 8) as u64) as *mut u64;
    let slots: [u64; SAVED_REGS + 1] = [
        0, // r15
        0, // r14
        0, // r13
        0, // r12
        0, // rbp
        entry as *const () as u64, // rbx
        task_trampoline as *const () as u64, // ret
    ];
    for (i, v) in slots.iter().enumerate() {
        // Safety: in range per the caller's contract.
        unsafe { frame.add(i).write(*v) };
    }
    VirtAddr::new(frame as u64)
}
