use alloc::boxed::Box;
use alloc::vec;
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

/// Written at the lowest bytes of every heap-backed stack. Heap stacks can't
/// have an unmapped guard page (see `Task::with_stack`), so an overflow is
/// only detectable after the fact by checking this wasn't overwritten.
const STACK_CANARY: u64 = 0xdead_beef_5ea1_c0de;

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

/// Minimal set for cooperative yielding (`scheduler::yield_now`). No `Blocked`
/// yet; add it once something can actually block (I/O, locks). No `Ord`: states have no
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
    /// allocator. `Some` for spawned tasks (`Task::with_stack`/`Task::spawn`).
    stack: Option<Box<[u8; STACK_SIZE]>>,
    state: TaskState,
}

impl Task {
    /// Call exactly once, to wrap the already-running `kernel_main` context
    /// so the first `switch` out of it has a `Task` to save `rsp` into.
    /// `scheduler::SCHEDULER`'s lazy initializer is the one caller.
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

    /// Creates a non-boot task owning a fresh heap stack, `Ready` but not yet
    /// runnable: `saved_rsp` is a null placeholder until a first frame is
    /// written with `context::init_stack`. Prefer `Task::spawn`, which does
    /// that; this split exists so the stack can be tested without switching.
    ///
    /// Stacks live on the kernel heap, not in separately mapped frames with
    /// an unmapped guard page. A guard page needs a page-aligned virtual
    /// range per task plus `MAPPER`/`FRAME_ALLOCATOR` locking and a fixed
    /// region carved out beside the heap; the heap is already mapped and
    /// zero extra machinery. The cost: an overflow silently corrupts the
    /// adjacent heap block instead of faulting. `check_canary` narrows that
    /// gap, but only when someone calls it.
    pub fn with_stack() -> Task {
        // `vec![0; n]` goes through `alloc_zeroed` straight into the heap.
        // `Box::new([0; STACK_SIZE])` in a debug build would first build the
        // whole 10 KiB array on the current (small) stack.
        let mut stack: Box<[u8; STACK_SIZE]> = vec![0u8; STACK_SIZE]
            .into_boxed_slice()
            .try_into()
            .expect("length is STACK_SIZE by construction");
        stack[..8].copy_from_slice(&STACK_CANARY.to_ne_bytes());
        Task {
            id: TaskId::new(),
            saved_rsp: VirtAddr::new(0),
            stack: Some(stack),
            state: TaskState::Ready,
        }
    }

    /// A `Ready` task whose first `switch` enters `entry`. Combines
    /// `with_stack` with `context::init_stack` so no caller can forget to
    /// point `saved_rsp` into the new stack.
    pub fn spawn(entry: extern "C" fn()) -> Task {
        let mut task = Task::with_stack();
        let top = task.stack_top().expect("with_stack always has a stack");
        // Safety: `top` is 16-aligned and the whole heap stack below it is
        // owned by `task`, which outlives any switch into it.
        task.saved_rsp = unsafe { crate::context::init_stack(top, entry) };
        task
    }

    /// Unique for the life of the kernel; IDs are never reused.
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Scheduler bookkeeping only; nothing here changes what the CPU runs.
    pub fn state(&self) -> &TaskState {
        &self.state
    }

    /// Plain setter: the scheduler, not `Task`, owns the legal transitions.
    pub fn set_state(&mut self, state: TaskState) {
        self.state = state;
    }

    /// The value to pass to `switch` as `new_rsp`.
    pub fn saved_rsp(&self) -> VirtAddr {
        self.saved_rsp
    }

    /// Raw pointer for `switch` to write through, so the scheduler can drop
    /// its lock before switching. Stays valid while the `Task` is alive and
    /// not moved; tasks are `Box`ed, so queue shuffling doesn't move it.
    pub fn saved_rsp_ptr(&mut self) -> *mut VirtAddr {
        &raw mut self.saved_rsp
    }

    /// One past the highest usable stack byte, rounded *down* to 16 bytes
    /// as the SysV ABI requires at call boundaries. `Box<[u8; N]>` only
    /// guarantees align 1, so up to 15 bytes at the top are left unused.
    /// `None` for the boot task, whose stack isn't ours.
    pub fn stack_top(&self) -> Option<VirtAddr> {
        let stack = self.stack.as_ref()?;
        let end = stack.as_ptr() as u64 + STACK_SIZE as u64;
        Some(VirtAddr::new(end & !0xf))
    }

    /// Lowest address of the stack allocation (where the canary lives).
    pub fn stack_bottom(&self) -> Option<VirtAddr> {
        let stack = self.stack.as_ref()?;
        Some(VirtAddr::new(stack.as_ptr() as u64))
    }

    /// `false` means the stack overflowed into its canary. Boot task has no
    /// heap stack to check, so it reports `true`.
    pub fn check_canary(&self) -> bool {
        match &self.stack {
            Some(stack) => stack[..8] == STACK_CANARY.to_ne_bytes(),
            None => true,
        }
    }
}
