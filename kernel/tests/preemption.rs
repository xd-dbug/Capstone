#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader_api::BootInfo;
use core::arch::asm;
use core::hint::black_box;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use kernel::BOOTLOADER_CONFIG;
use kernel::interrupts::{in_interrupt, ticks};
use kernel::scheduler::{ready_count, set_preemption, spawn, yield_now};
use x86_64::instructions::{hlt, interrupts::are_enabled};

fn test_kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let physical_memory_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("BOOTLOADER_CONFIG enables physical memory mapping");
    let memory_regions = &boot_info.memory_regions;

    if let Some(framebuffer) = boot_info.framebuffer.as_mut() {
        kernel::framebuffer::init(framebuffer);
    }
    // Preemption stays at its post-`init` default (on): it is what's under test.
    kernel::init(physical_memory_offset, memory_regions);
    test_main();
    loop {}
}

bootloader_api::entry_point!(test_kernel_main, config = &BOOTLOADER_CONFIG);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::test_panic_handler(info)
}

/// CPU-bound workers in the fairness test. Plus the boot task that is 5 tasks
/// round-robin, well inside `MAX_TASKS`.
const WORKERS: usize = 4;
/// Increments each worker performs. Sized so a worker needs many 10 ms time
/// slices (see `TIMER_HZ`) to finish: a run-to-completion scheduler could not
/// pass, and the interleaving checks below would have nothing to see.
const N: u64 = 400_000;
/// How often (in iterations) a worker peeks at the others and at
/// `in_interrupt`, to keep the hot loop dominated by the increments.
const PROBE_EVERY: u64 = 512;
/// Starvation budget, in ticks, from spawn to a worker's first increment.
/// Round-robin over `WORKERS + 1` tasks needs at most `WORKERS + 1` slices,
/// so this leaves 3x slack for a slow host.
const FIRST_RUN_BUDGET_TICKS: u64 = 3 * (WORKERS as u64 + 1);
/// Hard stop for any wait: 15 s at 100 Hz. A hang fails the test instead of
/// stalling the harness.
const WAIT_BUDGET_TICKS: u64 = 1500;

const U64_ZERO: AtomicU64 = AtomicU64::new(0);
const U64_MAX: AtomicU64 = AtomicU64::new(u64::MAX);
const FALSE: AtomicBool = AtomicBool::new(false);

/// Each worker's private progress, written with a plain load+store (a lost
/// update or a context mixed up between tasks would show as a wrong final
/// value, which `fetch_add` would hide only for the shared counter).
static COUNTERS: [AtomicU64; WORKERS] = [U64_ZERO; WORKERS];
/// Every worker's `fetch_add` target; the grand total must be exact.
static TOTAL: AtomicU64 = AtomicU64::new(0);
static FIRST_TICK: [AtomicU64; WORKERS] = [U64_MAX; WORKERS];
/// Probes where this worker, itself still mid-loop, saw another worker's
/// counter strictly between 0 and `N`: both were then unfinished at once,
/// which a run-to-completion order can never produce.
static SAW_OTHER_MID_FLIGHT: [AtomicU64; WORKERS] = [U64_ZERO; WORKERS];
static ENTRY_IF: [AtomicBool; WORKERS] = [FALSE; WORKERS];
static ENTRY_IN_INTERRUPT: [AtomicBool; WORKERS] = [FALSE; WORKERS];
/// Set if a worker ever saw `in_interrupt()` after being resumed from a
/// preemption (its depth must be restored to its own, 0, not left at the
/// ISR's).
static IN_INTERRUPT_MID_LOOP: AtomicBool = AtomicBool::new(false);

fn reset_fairness_state() {
    for i in 0..WORKERS {
        COUNTERS[i].store(0, Relaxed);
        FIRST_TICK[i].store(u64::MAX, Relaxed);
        SAW_OTHER_MID_FLIGHT[i].store(0, Relaxed);
        ENTRY_IF[i].store(false, Relaxed);
        ENTRY_IN_INTERRUPT[i].store(true, Relaxed);
    }
    TOTAL.store(0, Relaxed);
    IN_INTERRUPT_MID_LOOP.store(false, Relaxed);
}

/// Never calls `yield_now`: every switch away from it is a timer preemption.
fn worker(idx: usize) {
    ENTRY_IF[idx].store(are_enabled(), Relaxed);
    ENTRY_IN_INTERRUPT[idx].store(in_interrupt(), Relaxed);
    for i in 0..N {
        let n = COUNTERS[idx].load(Relaxed);
        COUNTERS[idx].store(n + 1, Relaxed);
        TOTAL.fetch_add(1, Relaxed);
        if i == 0 {
            FIRST_TICK[idx].store(ticks(), Relaxed);
        }
        if i % PROBE_EVERY == 0 {
            if in_interrupt() {
                IN_INTERRUPT_MID_LOOP.store(true, Relaxed);
            }
            for (j, c) in COUNTERS.iter().enumerate() {
                let v = c.load(Relaxed);
                if j != idx && v > 0 && v < N {
                    SAW_OTHER_MID_FLIGHT[idx].fetch_add(1, Relaxed);
                }
            }
        }
        // Keeps the loop from collapsing into one `COUNTERS[idx] = N` store.
        black_box(i);
    }
}

/// `extern "C" fn()` can't capture, so one entry fn per worker index.
macro_rules! worker_entry {
    ($name:ident, $idx:expr) => {
        extern "C" fn $name() {
            worker($idx);
        }
    };
}
worker_entry!(worker0, 0);
worker_entry!(worker1, 1);
worker_entry!(worker2, 2);
worker_entry!(worker3, 3);

/// Boot task waits with `hlt`, so it never yields either: its own switches
/// are preemptions too. Returns once every other task has exited, then
/// reaps their stacks (a preempted-away-from task never reaps on resume).
fn wait_for_all_tasks(budget_ticks: u64) {
    let start = ticks();
    while ready_count() > 0 {
        assert!(ticks() - start < budget_ticks, "tasks did not finish in time");
        hlt();
    }
    yield_now();
}

#[test_case]
fn test_cpu_bound_tasks_all_progress_and_total_is_exact() {
    reset_fairness_state();
    let spawned_at = ticks();
    for entry in [worker0, worker1, worker2, worker3] {
        spawn(entry).unwrap();
    }
    wait_for_all_tasks(WAIT_BUDGET_TICKS);

    // No lost preemption or increment: every private counter and the shared
    // total reached exactly the expected value.
    for i in 0..WORKERS {
        assert_eq!(COUNTERS[i].load(Relaxed), N, "worker {} lost increments", i);
    }
    assert_eq!(TOTAL.load(Relaxed), WORKERS as u64 * N);

    // No starvation: every worker got the CPU within the budget.
    for i in 0..WORKERS {
        let first = FIRST_TICK[i].load(Relaxed);
        assert_ne!(first, u64::MAX, "worker {} never ran", i);
        assert!(
            first - spawned_at <= FIRST_RUN_BUDGET_TICKS,
            "worker {} first ran {} ticks after spawn",
            i,
            first - spawned_at
        );
    }

    // Interleaving, not run-to-completion: workers saw each other unfinished.
    // The last worker to start can only have seen earlier ones mid-flight, so
    // count across workers rather than demanding it of each.
    let witnesses: u64 = SAW_OTHER_MID_FLIGHT.iter().map(|c| c.load(Relaxed)).sum();
    assert!(witnesses > 0, "no worker ever observed another mid-flight");
    let workers_that_saw = SAW_OTHER_MID_FLIGHT
        .iter()
        .filter(|c| c.load(Relaxed) > 0)
        .count();
    assert!(workers_that_saw >= 2, "only {} workers saw interleaving", workers_that_saw);

    // Per-task state: IF and ISR depth.
    for i in 0..WORKERS {
        assert!(ENTRY_IF[i].load(Relaxed), "worker {} started with IF=0", i);
        assert!(
            !ENTRY_IN_INTERRUPT[i].load(Relaxed),
            "worker {} started in interrupt context",
            i
        );
    }
    assert!(
        !IN_INTERRUPT_MID_LOOP.load(Relaxed),
        "a resumed worker still saw interrupt context"
    );
    assert!(!in_interrupt());
    assert!(are_enabled());
}

static ENTRY_IF_YIELD: AtomicBool = AtomicBool::new(false);
static ENTRY_IN_INTERRUPT_YIELD: AtomicBool = AtomicBool::new(true);

extern "C" fn records_entry_state() {
    ENTRY_IF_YIELD.store(are_enabled(), Relaxed);
    ENTRY_IN_INTERRUPT_YIELD.store(in_interrupt(), Relaxed);
}

/// The other way into a fresh task: from `yield_now`'s masked window rather
/// than from the ISR. Preemption off so the switch is certainly cooperative.
#[test_case]
fn test_fresh_task_entered_from_yield_has_interrupts_enabled() {
    set_preemption(false);
    spawn(records_entry_state).unwrap();
    while ready_count() > 0 {
        yield_now();
    }
    set_preemption(true);
    assert!(ENTRY_IF_YIELD.load(Relaxed), "fresh task started with IF=0");
    assert!(!ENTRY_IN_INTERRUPT_YIELD.load(Relaxed));
    // The boot task's own IF survived its cooperative yields.
    assert!(are_enabled());
}

static RAN: AtomicBool = AtomicBool::new(false);

extern "C" fn sets_ran() {
    RAN.store(true, Relaxed);
}

/// With preemption off, ticks keep counting but never switch, which is what
/// the deterministic tests rely on.
#[test_case]
fn test_preemption_off_never_switches() {
    RAN.store(false, Relaxed);
    set_preemption(false);
    spawn(sets_ran).unwrap();
    let start = ticks();
    while ticks() - start < 5 {
        hlt();
    }
    assert!(!RAN.load(Relaxed), "a task ran with preemption off");
    assert_eq!(ready_count(), 1);
    set_preemption(true);
    wait_for_all_tasks(WAIT_BUDGET_TICKS);
    assert!(RAN.load(Relaxed), "the task never ran once preemption came back");
}

/// Registers the preemption test pins, as 32-bit immediates (sign-extended to
/// 64 bits by `mov`/`cmp`, so a negative base exercises the upper halves).
const REG_SPIN_LIMIT: u64 = 3_000_000;
static REG_BAD: AtomicBool = AtomicBool::new(false);
/// One per task: `sym` operands name a static, not an array element. The
/// asm bumps its own with a plain `inc` (single writer), the boot task reads
/// all three to see the tasks mid-flight at once.
static REG_COUNT0: AtomicU64 = AtomicU64::new(0);
static REG_COUNT1: AtomicU64 = AtomicU64::new(0);
static REG_COUNT2: AtomicU64 = AtomicU64::new(0);
static REG_DONE: AtomicUsize = AtomicUsize::new(0);

/// Loads every GPR except rsp with a task-specific value and spins, checking
/// all of them against it on each pass. A timer preemption between any two
/// instructions leaves only the interrupt frame, the handler's saves and
/// `switch`'s saves to carry them across; any register or the flags from the
/// `cmp` feeding a `jne` not restored shows up as a mismatch. `rbx` and `rbp`
/// can't be asm operands, so they are saved and restored by hand.
macro_rules! register_task {
    ($name:ident, $count:ident, $base:expr) => {
        extern "C" fn $name() {
            // Safety: only registers (declared clobbered, rbx/rbp restored by
            // hand) and the two statics via `sym` are touched; the stack
            // pointer is balanced.
            unsafe {
                asm!(
                    "push rbx",
                    "push rbp",
                    "mov rax, {b}",
                    "mov rcx, {b}+1",
                    "mov rdx, {b}+2",
                    "mov rbx, {b}+3",
                    "mov rsi, {b}+4",
                    "mov rdi, {b}+5",
                    "mov rbp, {b}+6",
                    "mov r8, {b}+7",
                    "mov r9, {b}+8",
                    "mov r10, {b}+9",
                    "mov r11, {b}+10",
                    "mov r12, {b}+11",
                    "mov r13, {b}+12",
                    "mov r14, {b}+13",
                    "mov r15, {b}+14",
                    "2:",
                    "cmp rax, {b}",
                    "jne 3f",
                    "cmp rcx, {b}+1",
                    "jne 3f",
                    "cmp rdx, {b}+2",
                    "jne 3f",
                    "cmp rbx, {b}+3",
                    "jne 3f",
                    "cmp rsi, {b}+4",
                    "jne 3f",
                    "cmp rdi, {b}+5",
                    "jne 3f",
                    "cmp rbp, {b}+6",
                    "jne 3f",
                    "cmp r8, {b}+7",
                    "jne 3f",
                    "cmp r9, {b}+8",
                    "jne 3f",
                    "cmp r10, {b}+9",
                    "jne 3f",
                    "cmp r11, {b}+10",
                    "jne 3f",
                    "cmp r12, {b}+11",
                    "jne 3f",
                    "cmp r13, {b}+12",
                    "jne 3f",
                    "cmp r14, {b}+13",
                    "jne 3f",
                    "cmp r15, {b}+14",
                    "jne 3f",
                    "inc qword ptr [rip + {count}]",
                    "cmp qword ptr [rip + {count}], {limit}",
                    "jb 2b",
                    "jmp 4f",
                    "3:",
                    "mov byte ptr [rip + {bad}], 1",
                    "4:",
                    "pop rbp",
                    "pop rbx",
                    b = const $base,
                    limit = const REG_SPIN_LIMIT,
                    count = sym $count,
                    bad = sym REG_BAD,
                    out("rax") _, out("rcx") _, out("rdx") _, out("rsi") _,
                    out("rdi") _, out("r8") _, out("r9") _, out("r10") _,
                    out("r11") _, out("r12") _, out("r13") _, out("r14") _,
                    out("r15") _,
                );
            }
            REG_DONE.fetch_add(1, Relaxed);
        }
    };
}

// Positive and negative bases: the negative ones sign-extend to values with
// all-ones upper halves, so a save/restore that truncated to 32 bits fails.
register_task!(register_task0, REG_COUNT0, 0x1000);
register_task!(register_task1, REG_COUNT1, -0x2000);
register_task!(register_task2, REG_COUNT2, 0x3000);

/// The full-context-save proof: three tasks hold different values in every
/// general-purpose register while being preempted over and over, and none
/// sees another's (or a clobbered) value.
#[test_case]
fn test_all_registers_survive_preemption() {
    let counts = [&REG_COUNT0, &REG_COUNT1, &REG_COUNT2];
    for c in counts {
        c.store(0, Relaxed);
    }
    REG_BAD.store(false, Relaxed);
    REG_DONE.store(0, Relaxed);

    for entry in [register_task0, register_task1, register_task2] {
        spawn(entry).unwrap();
    }
    // Sample from the boot task each time it is resumed: if two tasks are
    // ever both strictly unfinished in one sample, they really ran
    // interleaved rather than one after the other.
    let start = ticks();
    let mut interleaved_samples = 0;
    while ready_count() > 0 {
        assert!(ticks() - start < WAIT_BUDGET_TICKS, "register tasks did not finish");
        let mid_flight = counts
            .iter()
            .filter(|c| (1..REG_SPIN_LIMIT).contains(&c.load(Relaxed)))
            .count();
        if mid_flight >= 2 {
            interleaved_samples += 1;
        }
        hlt();
    }
    yield_now();

    assert!(!REG_BAD.load(Relaxed), "a register changed across preemption");
    assert_eq!(REG_DONE.load(Relaxed), 3);
    for c in counts {
        assert_eq!(c.load(Relaxed), REG_SPIN_LIMIT);
    }
    assert!(interleaved_samples > 0, "register tasks never ran interleaved");
}

const CHURN_TASKS: usize = 3;
const CHURN_ROUNDS: u64 = 3_000;
static CHURN_OK: AtomicUsize = AtomicUsize::new(0);

/// Allocates and frees constantly, so some ticks land while the heap is busy.
/// The ISR must neither deadlock on the heap lock nor allocate itself
/// (`GuardedHeap` panics in interrupt context): the heap lock is only ever
/// held with interrupts masked, so a tick is deferred past it.
extern "C" fn churn() {
    for round in 0..CHURN_ROUNDS {
        let v: Vec<u64> = (0..16).map(|x| x ^ round).collect();
        let sum: u64 = black_box(&v).iter().sum();
        let expected: u64 = (0..16u64).map(|x| x ^ round).sum();
        assert_eq!(sum, expected);
    }
    CHURN_OK.fetch_add(1, Relaxed);
}

#[test_case]
fn test_heap_churn_under_preemption() {
    CHURN_OK.store(0, Relaxed);
    let start = ticks();
    for _ in 0..CHURN_TASKS {
        spawn(churn).unwrap();
    }
    // The boot task allocates too, racing the workers for the heap.
    while ready_count() > 0 {
        assert!(ticks() - start < WAIT_BUDGET_TICKS, "churn tasks did not finish");
        let v: Vec<u8> = black_box(alloc::vec![7u8; 64]);
        assert_eq!(v.len(), 64);
        drop(v);
    }
    yield_now();
    assert_eq!(CHURN_OK.load(Relaxed), CHURN_TASKS);
}

const MIXED_YIELDERS: usize = 2;
const MIXED_SPINNERS: usize = 2;
/// Each yielder's round count. Every round can cost both spinners a full
/// 10 ms slice, so this stays small to keep the test around a second.
const MIXED_ROUNDS: u64 = 40;
static YIELDER_ROUNDS: [AtomicU64; MIXED_YIELDERS] = [U64_ZERO; MIXED_YIELDERS];
static YIELDERS_DONE: AtomicUsize = AtomicUsize::new(0);
static SPIN_COUNT: [AtomicU64; MIXED_SPINNERS] = [U64_ZERO; MIXED_SPINNERS];
static SPIN_LOST: AtomicBool = AtomicBool::new(false);
/// Set if any task, after being resumed by either path, saw interrupt
/// context or IF=0.
static MIXED_BAD: AtomicBool = AtomicBool::new(false);
/// Yields across which a spinner advanced: the spinners (which only ever
/// leave the CPU by preemption) ran between this yielder's switch-out and its
/// resume.
static MIXED_WITNESSES: AtomicU64 = AtomicU64::new(0);

fn spin_total() -> u64 {
    SPIN_COUNT.iter().map(|c| c.load(Relaxed)).sum()
}

fn check_resumed_state() {
    if in_interrupt() || !are_enabled() {
        MIXED_BAD.store(true, Relaxed);
    }
}

/// Cooperative: switches out only via `yield_now`, but can be resumed either
/// by another `yield_now` or by the timer ISR preempting a spinner, and can
/// hand the CPU to a spinner that was itself preempted.
fn yielder(idx: usize) {
    for _ in 0..MIXED_ROUNDS {
        let before = spin_total();
        yield_now();
        check_resumed_state();
        if spin_total() > before {
            MIXED_WITNESSES.fetch_add(1, Relaxed);
        }
        YIELDER_ROUNDS[idx].fetch_add(1, Relaxed);
    }
    YIELDERS_DONE.fetch_add(1, Relaxed);
}

/// CPU-bound until both yielders finish, so the two kinds of task stay mixed
/// for the whole run. The plain load+store counter must equal the local count
/// at the end: a context restored into the wrong task would break that.
fn spinner(idx: usize) {
    let mut local = 0u64;
    while YIELDERS_DONE.load(Relaxed) < MIXED_YIELDERS {
        let n = SPIN_COUNT[idx].load(Relaxed);
        SPIN_COUNT[idx].store(n + 1, Relaxed);
        local += 1;
        if local % PROBE_EVERY == 0 {
            check_resumed_state();
        }
        black_box(local);
    }
    if SPIN_COUNT[idx].load(Relaxed) != local {
        SPIN_LOST.store(true, Relaxed);
    }
}

extern "C" fn mixed_yielder0() {
    yielder(0);
}
extern "C" fn mixed_yielder1() {
    yielder(1);
}
extern "C" fn mixed_spinner0() {
    spinner(0);
}
extern "C" fn mixed_spinner1() {
    spinner(1);
}

/// Mixes the two suspend/resume mechanisms: ISR -> cooperative yielder and
/// yielder -> preempted spinner. Each hands over `ISR_DEPTH` and IF
/// differently (`switch_from_isr`'s swap/restore and `iretq` vs.
/// `without_interrupts`), and the other tests keep them apart.
#[test_case]
fn test_mixed_cooperative_and_preempted_tasks() {
    for i in 0..MIXED_YIELDERS {
        YIELDER_ROUNDS[i].store(0, Relaxed);
    }
    for i in 0..MIXED_SPINNERS {
        SPIN_COUNT[i].store(0, Relaxed);
    }
    YIELDERS_DONE.store(0, Relaxed);
    SPIN_LOST.store(false, Relaxed);
    MIXED_BAD.store(false, Relaxed);
    MIXED_WITNESSES.store(0, Relaxed);

    for entry in [mixed_spinner0, mixed_yielder0, mixed_spinner1, mixed_yielder1] {
        spawn(entry).unwrap();
    }
    wait_for_all_tasks(WAIT_BUDGET_TICKS);

    for i in 0..MIXED_YIELDERS {
        assert_eq!(YIELDER_ROUNDS[i].load(Relaxed), MIXED_ROUNDS, "yielder {} lost rounds", i);
    }
    for i in 0..MIXED_SPINNERS {
        assert!(SPIN_COUNT[i].load(Relaxed) > 0, "spinner {} never ran", i);
    }
    assert!(!SPIN_LOST.load(Relaxed), "a spinner's counter disagreed with its own count");
    assert!(!MIXED_BAD.load(Relaxed), "a resumed task saw interrupt context or IF=0");
    assert!(MIXED_WITNESSES.load(Relaxed) > 0, "spinners never ran between a yield and its resume");
    assert!(!in_interrupt());
    assert!(are_enabled());
}
