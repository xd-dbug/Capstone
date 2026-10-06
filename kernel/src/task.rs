use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::VirtAddr;

/// Atomic rather than `static mut` (project convention) or a `Mutex` (an
/// interrupt handler spinning on a lock the interrupted code already holds
/// would deadlock). `fetch_add` is a single `lock xadd`, so concurrent
/// creators (an interrupt handler, later another CPU) can never get the same
/// ID.
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(0);

/// Kept small because the whole kernel heap is 100 KiB
/// (`allocator::HEAP_SIZE`) and W2 needs 3+ concurrent tasks, so several
/// stacks must fit with room left for other allocations. Debug builds and
/// `core::fmt` are stack-hungry: a mysterious crash while logging from a
/// thread is the first sign this is too small.
pub const STACK_SIZE: usize = 10 * 1024;

/// Newtype so IDs can't be mixed up with other `u64`s. `Ord` is meaningful
/// (lower = created earlier) and lets W2 use it as a `BTreeMap` key.
#[derive(Debug, PartialEq, Eq, Ord, Copy, Clone, PartialOrd)]
pub struct TaskId(u64);
impl TaskId {
    /// Private so IDs only come from `NEXT_TASK_ID`, via `Task` constructors.
    fn new() -> TaskId {
        // `fetch_add` returns the *old* value, which is the one this caller
        // claimed. `Relaxed` suffices: only uniqueness matters, the counter
        // doesn't publish any other memory. A `u64` never realistically wraps.
        TaskId(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Minimal set for cooperative yielding (#11). No `Blocked` yet; add it once
/// something can actually block (I/O, locks). No `Ord`: states have no
/// meaningful ordering, only equality.
#[derive(PartialEq, Eq, Debug)]
pub enum TaskState {
    /// Can be picked to run next.
    Ready,
    /// The task whose `saved_rsp` `switch` writes into.
    Running,
    /// Its function returned. Must never be switched into again: there is
    /// nothing on its stack to resume.
    Finished,
}

/// Derives nothing (no `Clone`/`Copy`) on purpose: one owner per stack `Box`.
///
/// A task's stack must never be freed by code running on that stack. The
/// allocator writes hole metadata into freed memory and the next allocation
/// can hand it out while `rsp` still points into it. Freeing a `Finished`
/// task's stack is the job of whichever task runs next, after the switch has
/// moved `rsp` off it.
pub struct Task {
    id: TaskId,
    /// The only saved CPU state, no register array. The switch is
    /// cooperative (a task calls `switch` itself), so per the SysV calling
    /// convention only the callee-saved registers (rbx, rbp, r12-r15) must
    /// survive. `switch` pushes them onto the outgoing task's own stack, then
    /// stores `rsp` here; restoring is load `rsp`, pop in reverse order,
    /// `ret` (popping the return address pushed by the original `call`). So
    /// this *is* the pointer to the saved registers. Virtual because `rsp` is
    /// always virtual once paging is on; all kernel threads share one page
    /// table at Layer 4, so it stays valid across switches. Preemptive
    /// switching from a timer IRQ would need all registers saved (W2).
    saved_rsp: VirtAddr,
    /// `None` for the boot task, which runs on the bootloader's stack. That
    /// memory isn't from the kernel heap and must never be handed to the heap
    /// allocator. `Some` for spawned tasks (#13).
    stack: Option<Box<[u8; STACK_SIZE]>>,
    state: TaskState,
}

impl Task {
    /// Call exactly once, to wrap the already-running `kernel_main` context
    /// so the first `switch` out of it has a `Task` to save `rsp` into.
    pub fn boot() -> Task {
        Task {
            id: TaskId::new(),
            // Placeholder the first switch overwrites. 0 so a bug that
            // switches *into* the boot task before it was ever saved faults
            // at once on the unmapped null page instead of running garbage.
            saved_rsp: VirtAddr::new(0),
            stack: None,
            // Already executing.
            state: TaskState::Running,
        }
    }
}
