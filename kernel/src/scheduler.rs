use crate::context::switch;
use crate::task::{Task, TaskId, TaskState};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

/// Round-robin over any number of tasks: `current` plus a FIFO of the rest.
/// Tasks are `Box`ed so the address `switch` writes `saved_rsp` through stays
/// put while a task moves between `current`, `ready` and `finished`.
struct Scheduler {
    current: Box<Task>,
    ready: VecDeque<Box<Task>>,
    /// Tasks whose function returned. Their stacks can't be freed by the task
    /// still running on them, so the next task to run reaps them.
    finished: Vec<Box<Task>>,
}

lazy_static! {
    /// Lazy (not `OnceCell`) so the first call wraps whoever is running, the
    /// boot task, as `current`. Must first be touched after the heap is up;
    /// `init` forces that from `kernel::init` so it never happens implicitly.
    static ref SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler {
        current: Box::new(Task::boot()),
        ready: VecDeque::new(),
        finished: Vec::new(),
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
/// immediately.
pub fn spawn(entry: extern "C" fn()) -> TaskId {
    let task = Box::new(Task::spawn(entry));
    let id = task.id();
    with_scheduler(|s| s.ready.push_back(task));
    id
}

/// Number of tasks other than the caller that could run. Lets the boot task
/// loop `yield_now` until all spawned work is done.
pub fn ready_count() -> usize {
    with_scheduler(|s| s.ready.len())
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
    let switch_args = with_scheduler(|s| {
        let mut next = s.ready.pop_front()?;
        assert!(s.current.check_canary(), "stack overflow in task {:?}", s.current.id());
        assert!(next.check_canary(), "stack overflow in task {:?}", next.id());
        debug_assert_eq!(*next.state(), TaskState::Ready);

        let new_rsp = next.saved_rsp();
        next.set_state(TaskState::Running);
        let mut prev = core::mem::replace(&mut s.current, next);
        prev.set_state(TaskState::Ready);
        // Taken before the move into the queue: moving a `Box` keeps the
        // pointee where it is.
        let old_rsp = prev.saved_rsp_ptr();
        s.ready.push_back(prev);
        Some((old_rsp, new_rsp))
    });
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

/// Called from `task_returned`: retires the running task and never returns.
pub(crate) fn exit_current() -> ! {
    let (old_rsp, new_rsp) = with_scheduler(|s| {
        // The boot task never returns, so it is always queued while another
        // task runs; an empty queue means the boot task was lost.
        let mut next = s
            .ready
            .pop_front()
            .expect("last runnable task finished; no boot task to return to");
        // Checked here too: a task that overflowed and then returned would
        // otherwise never pass through `yield_now`'s check again.
        assert!(s.current.check_canary(), "stack overflow in task {:?}", s.current.id());
        assert!(next.check_canary(), "stack overflow in task {:?}", next.id());
        let new_rsp = next.saved_rsp();
        next.set_state(TaskState::Running);
        let mut prev = core::mem::replace(&mut s.current, next);
        prev.set_state(TaskState::Finished);
        let old_rsp = prev.saved_rsp_ptr();
        // Kept alive (not dropped) because we are still running on its stack
        // until `switch` below moves rsp away.
        s.finished.push(prev);
        (old_rsp, new_rsp)
    });
    // Safety: as in `yield_now`; `old_rsp` stays valid because the task sits
    // in `finished` until the next task's `reap`, which runs after this
    // switch. The saved frame is never resumed: nothing queues a `Finished` task.
    unsafe { switch(old_rsp, new_rsp) };
    unreachable!("a finished task was switched back into");
}

/// Frees stacks of finished tasks. Runs on the resumed task, so `rsp` is
/// guaranteed to be off the stacks being freed. A task entered for the first
/// time skips this until its own first `yield_now`; that only delays the free.
fn reap() {
    // Drop outside the lock: freeing takes the heap lock and needn't nest.
    let dead = with_scheduler(|s| core::mem::take(&mut s.finished));
    drop(dead);
}
