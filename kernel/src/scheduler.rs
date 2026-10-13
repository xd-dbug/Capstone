use crate::context::switch;
use crate::task::{Task, TaskId, TaskState};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts::without_interrupts;

/// Most tasks that can exist at once, boot task included. The heap is 100 KiB
/// and each spawned task takes a 10 KiB stack, so 8 tasks = 7 stacks = 70 KiB
/// leaves ~30 KiB for the run queue, `Box<Task>`s and everything else. A 9th
/// would leave too little to be safe. Also sizes the preallocated `ready` and
/// `finished` queues, so pushing onto them never grows (allocates).
pub const MAX_TASKS: usize = 8;

/// Why `spawn` refused. Both are expected outcomes of a small fixed heap, not
/// bugs, so they are values rather than panics.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SpawnError {
    /// `MAX_TASKS` tasks already exist.
    TooManyTasks,
    /// The heap couldn't hold another stack (e.g. finished tasks' stacks not
    /// yet reaped, or other allocations eating the budget).
    OutOfMemory,
}

/// `switch` arguments produced by `select_next`: where to save the outgoing
/// rsp, and the rsp to resume.
pub(crate) type SwitchArgs = (*mut VirtAddr, VirtAddr);

/// Round-robin over any number of tasks: `current` plus a FIFO of the rest.
/// Tasks are `Box`ed so the address `switch` writes `saved_rsp` through stays
/// put while a task moves between `current`, `ready` and `finished`.
struct Scheduler {
    current: Box<Task>,
    ready: VecDeque<Box<Task>>,
    /// Tasks whose function returned or called `exit`. Their stacks can't be
    /// freed by the task still running on them, so the next task to run reaps
    /// them.
    finished: Vec<Box<Task>>,
}

impl Scheduler {
    /// Live tasks: `current`, the queue, and finished-but-unreaped ones (their
    /// stacks still occupy the heap, so they count against the budget).
    fn is_full(&self) -> bool {
        1 + self.ready.len() + self.finished.len() >= MAX_TASKS
    }

    /// The one round-robin step behind both `select_next` and `exit`: pops
    /// the front of `ready`, makes it `current`, and files the old `current`
    /// per `outgoing`. Returns the `switch` arguments, or `None` if nothing
    /// else is ready.
    fn rotate(&mut self, outgoing: Outgoing) -> Option<SwitchArgs> {
        let mut next = self.ready.pop_front()?;
        // Checked on every rotation, exit included: a task that overflowed
        // and then exited would otherwise never be checked again.
        assert!(self.current.check_canary(), "stack overflow in task {:?}", self.current.id());
        assert!(next.check_canary(), "stack overflow in task {:?}", next.id());
        debug_assert_eq!(*next.state(), TaskState::Ready);

        let new_rsp = next.saved_rsp();
        next.set_state(TaskState::Running);
        let mut prev = core::mem::replace(&mut self.current, next);
        // Taken before the move into a queue: moving a `Box` keeps the
        // pointee where it is.
        let old_rsp = prev.saved_rsp_ptr();
        // Neither push can grow: both queues are preallocated for
        // `MAX_TASKS`, `ready` just lost the entry popped above, and
        // `is_full` counts `finished` toward the cap. A grow would allocate,
        // which an ISR must not do and which could fail mid-exit.
        match outgoing {
            Outgoing::Requeue => {
                prev.set_state(TaskState::Ready);
                debug_assert!(self.ready.len() < self.ready.capacity());
                self.ready.push_back(prev);
            }
            Outgoing::Retire => {
                // Kept alive (not dropped): we are still running on its stack
                // until the caller's `switch` moves rsp away.
                prev.set_state(TaskState::Finished);
                debug_assert!(self.finished.len() < self.finished.capacity());
                self.finished.push(prev);
            }
        }
        Some((old_rsp, new_rsp))
    }
}

/// What `Scheduler::rotate` does with the task it switches away from.
#[derive(Clone, Copy)]
enum Outgoing {
    /// Back of `ready`, to run again (yield, and later preemption).
    Requeue,
    /// Into `finished`, for whoever runs next to reap (exit).
    Retire,
}

lazy_static! {
    /// Lazy (not `OnceCell`) so the first call wraps whoever is running, the
    /// boot task, as `current`. Must first be touched after the heap is up;
    /// `init` forces that from `kernel::init` so it never happens implicitly.
    static ref SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler {
        current: Box::new(Task::boot()),
        // One slot per task other than `current`, so `select_next` (which
        // an ISR will call) can never need to grow it.
        ready: VecDeque::with_capacity(MAX_TASKS - 1),
        // Same reasoning: `exit` pushes here under the lock, and an
        // allocation there could fail mid-exit.
        finished: Vec::with_capacity(MAX_TASKS - 1),
    });
}

/// Locking rule for this module: the lock is only ever held to shuffle
/// queues and is always dropped *before* `switch`. The resumed task would
/// otherwise spin on a lock owned by a task that cannot run. Interrupts are
/// masked while it is held, in case a future IRQ handler (preemption) takes
/// it: none does today, but one that did would deadlock against the code it
/// interrupted.
fn with_scheduler<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    without_interrupts(|| f(&mut SCHEDULER.lock()))
}

/// Wraps the running context as the boot task. Called from `kernel::init`
/// right after the heap is up, so the boot task deterministically gets the
/// first `TaskId` (otherwise the first `spawn` would claim an ID before the
/// lazy initializer ran) and nothing allocates in a lazy init mid-yield.
pub fn init() {
    lazy_static::initialize(&SCHEDULER);
}

/// Queues a new task to run on a later `yield_now`. It does not run
/// immediately. Fails instead of panicking when `MAX_TASKS` tasks exist or the
/// heap can't hold the stack.
///
/// No allocation or free happens under the lock (the push never grows the
/// preallocated queue). The cap is checked twice: up front, so the common
/// refusal costs no memory or `TaskId`, and again under the lock so the check
/// and the push are one atomic step. A refusal at the second check (or a
/// failed `Box`) does burn the `TaskId`; IDs are only for identification, so
/// a gap is harmless.
pub fn spawn(entry: extern "C" fn()) -> Result<TaskId, SpawnError> {
    if with_scheduler(|s| s.is_full()) {
        return Err(SpawnError::TooManyTasks);
    }
    let task = Task::spawn(entry).map_err(|_| SpawnError::OutOfMemory)?;
    let id = task.id();
    // Fallible: a failed `Box::new` would abort the kernel.
    let task = Box::try_new(task).map_err(|_| SpawnError::OutOfMemory)?;
    let rejected = with_scheduler(|s| {
        if s.is_full() {
            return Some(task);
        }
        s.ready.push_back(task);
        None
    });
    match rejected {
        // Dropped here, after the lock: freeing the stack takes the heap lock
        // and needn't nest inside the scheduler's.
        Some(_task) => Err(SpawnError::TooManyTasks),
        None => Ok(id),
    }
}

/// Number of tasks other than the caller that could run. Lets the boot task
/// loop `yield_now` until all spawned work is done.
pub fn ready_count() -> usize {
    with_scheduler(|s| s.ready.len())
}

/// The one round-robin selection step, shared by `yield_now` and (later) the
/// timer ISR: pops the front of the queue, makes it `current`, and requeues
/// the old `current` at the back. Returns the `switch` arguments, or `None` if
/// nothing else is ready.
///
/// Never allocates: the queue was preallocated for `MAX_TASKS` and this only
/// pops one entry then pushes one back. It only takes the scheduler lock
/// (with interrupts masked), so lock- and heap-wise it is safe to call from an
/// interrupt handler. It does not itself switch; the caller drops everything
/// and calls `switch`.
///
/// Once it returns, `current` already names the next task while the CPU is
/// still on the old one's stack, and the old task's `saved_rsp` isn't written
/// until `switch`. Nothing may select again in that window, so once the timer
/// ISR can preempt (W2), callers must keep interrupts off from here through
/// `switch`.
pub(crate) fn select_next() -> Option<SwitchArgs> {
    with_scheduler(|s| s.rotate(Outgoing::Requeue))
}

/// Moves the caller to the back of the queue and runs the front task. Returns
/// (later) once every other task ahead of it has yielded or finished; returns
/// at once if nothing else is ready.
///
/// Must be called with interrupts enabled: `switch` doesn't save RFLAGS, so
/// the interrupt flag isn't per-task, and calling this with IF=0 would hand
/// the next task a CPU with interrupts silently off. Preemption (W2) will
/// need IF in the saved state instead.
pub fn yield_now() {
    debug_assert!(
        x86_64::instructions::interrupts::are_enabled(),
        "yield_now called with interrupts disabled"
    );
    let switch_args = select_next();
    let Some((old_rsp, new_rsp)) = switch_args else {
        return;
    };
    // Safety: `old_rsp` points into the heap-allocated `Task` now in `ready`
    // (alive, boxed, never freed while queued). `new_rsp` came from a
    // `Ready` task's `init_stack` or earlier `switch`. No lock is held.
    unsafe { switch(old_rsp, new_rsp) };
    // Only reached once someone switches back to us, now on our own stack.
    reap();
}

/// Number of exited tasks whose stacks are still waiting to be freed. Exists
/// so tests can observe that `reap` ran promptly; not useful for scheduling.
pub fn finished_count() -> usize {
    with_scheduler(|s| s.finished.len())
}

/// Ends the calling task for good, from any call depth. Also what a task's
/// function returning ends up calling, via `task_returned`.
///
/// Destructors of live locals in the abandoned frames do NOT run (the stack is
/// discarded, not unwound), so anything owning a heap allocation or a lock
/// guard in those frames leaks or stays locked. Drop such values before
/// calling this.
///
/// Must not be called on the boot task: it runs on the bootloader's stack and
/// is the scheduler's fallback when the queue would otherwise be empty. Like
/// `yield_now`, needs interrupts enabled since `switch` doesn't save RFLAGS.
pub fn exit() -> ! {
    debug_assert!(
        x86_64::instructions::interrupts::are_enabled(),
        "exit called with interrupts disabled"
    );
    let (old_rsp, new_rsp) = with_scheduler(|s| {
        assert!(s.current.stack_top().is_some(), "the boot task cannot exit");
        // The boot task never exits, so it is always queued while another
        // task runs; an empty queue means the boot task was lost.
        s.rotate(Outgoing::Retire)
            .expect("last runnable task finished; no boot task to return to")
    });
    // Safety: as in `yield_now`; `old_rsp` stays valid because the task sits
    // in `finished` until the next task's `reap`, which runs after this
    // switch. The saved frame is never resumed: nothing queues a `Finished` task.
    unsafe { switch(old_rsp, new_rsp) };
    unreachable!("a finished task was switched back into");
}

/// Frees stacks of finished tasks. Runs on the resumed task, so `rsp` is
/// guaranteed to be off the stacks being freed.
///
/// Only ever called from mainline task context: after `switch` in
/// `yield_now`, and from `context::task_started` before a fresh task's
/// `entry`. Freeing takes the heap lock, which an ISR must never do (it may
/// have interrupted the allocator holding that lock, a deadlock;
/// `allocator::GuardedHeap` panics if it tries), so reaping must stay out of
/// interrupt handlers. When timer preemption lands, the handler should only
/// pick and switch; this still runs on the resumed task's mainline path.
pub(crate) fn reap() {
    // One at a time, each dropped outside the lock: freeing takes the heap
    // lock and needn't nest. Popping (not `mem::take`) keeps `finished`'s
    // preallocated capacity, so `exit` never has to grow it.
    while let Some(dead) = with_scheduler(|s| s.finished.pop()) {
        drop(dead);
    }
}
