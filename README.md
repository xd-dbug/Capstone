# SealOS

A hobby x86_64 kernel written in Rust, built as pre-capstone / capstone (PRO390) coursework. Boots via UEFI in QEMU, with a security-conscious design target (privilege separation, syscalls, and — as stretch goals — capability-based access control and ASLR).

## Status

**Working right now:**
- Boots via UEFI (OVMF firmware) in QEMU to a graphical framebuffer
- Text output to the framebuffer (`println!`/`print!`) via software-rendered glyphs — not legacy VGA text mode, which isn't available under UEFI/GOP boot (see `docs/superpowers/specs/2026-07-09-vga-text-mode-design.md` for why)
- Serial output over COM1 (`serial_println!`/`serial_print!`) via `uart_16550`
- GDT with a dedicated Interrupt Stack Table (IST) entry for double faults
- IDT with breakpoint and double-fault handlers wired up
- A custom `#[no_std]` test harness: unit tests inside `kernel/src/`, plus integration tests (`basic_boot`, `should_panic`, `stack_overflow`) that boot a real kernel image in QEMU and report pass/fail over the `isa-debug-exit` device
- `cargo run` and `cargo test` both work end-to-end, cross-compiling the kernel and launching QEMU automatically

**Not implemented yet:**
- Hardware interrupts beyond breakpoint/double-fault — no PIC remapping, no timer, no keyboard (the `pic8259` dependency is present but unused so far)
- Physical memory management, paging, and a heap allocator (Layers 2–3)
- Process scheduling, privilege separation (Ring 3), syscalls, and a shell (capstone-proper deliverables)

If something in the code looks unfinished or stubbed, it probably is — this project is under active, weekly development. Check the commit history rather than assuming this list is current by the time you read it.

## Architecture

Two independent Cargo projects live in this repo:

```
.
├── Cargo.toml          # "runner" — a std host binary
├── build.rs             # cross-compiles kernel/ as part of the runner's own build
├── src/main.rs           # builds a UEFI disk image and launches QEMU
└── kernel/
    ├── Cargo.toml       # "kernel" — the actual no_std, no_main OS
    ├── .cargo/config.toml   # points kernel's `cargo test` runner back at ../target/debug/runner
    ├── x86_64-seal_os.json  # custom target spec (softfloat, no SSE/MMX, no red zone)
    └── src/
        ├── main.rs       # kernel entry point
        ├── lib.rs        # shared init, panic handling, test harness
        ├── framebuffer.rs
        ├── serial.rs
        ├── gdt.rs
        └── interrupts.rs
```

**Why two crates:** `kernel` needs its own target spec, its own `build-std` configuration, and `no_std`. Keeping it a fully separate Cargo project (its own `target/` directory, its own `.cargo/config.toml`) avoids a workspace deadlock where a nested `cargo build` inside `build.rs` blocks on the same build lock as the outer build.

**The `runner` crate does double duty:**
1. Run directly (`cargo run` at the repo root) — `build.rs` cross-compiles the kernel, and `runner` boots the resulting ELF in QEMU.
2. Run indirectly, as the `runner` configured in `kernel/.cargo/config.toml` — when you `cargo test` inside `kernel/`, Cargo invokes the prebuilt `../target/debug/runner` binary with the test ELF's path as an argument. `runner` detects test binaries (they live under a `deps/` directory) and wires up the `isa-debug-exit` QEMU device plus headless serial output, translating the VM's exit code back into a `cargo test` pass/fail.

**UEFI only.** BIOS boot is not supported — `bootloader = "0.11"` is configured with `default-features = false, features = ["uefi"]`, matching the framebuffer-only design above.

## Requirements

- **Rust nightly**, pinned via `rust-toolchain.toml` (currently `nightly-2026-07-19`) with the `rust-src` component — `rustup` will fetch this automatically the first time you build
- **QEMU** (`qemu-system-x86_64` on your `PATH`)
- **OVMF UEFI firmware** — `src/main.rs` currently expects it at `/usr/share/OVMF/OVMF_CODE_4M.fd` and `/usr/share/OVMF/OVMF_VARS_4M.fd` (the standard location on Arch/Debian-based distros via the `ovmf` or `edk2-ovmf` package). If your firmware lives elsewhere, you'll need to edit those constants directly for now — there's no environment-variable override yet.

## Build & run

```sh
# Boot the kernel in QEMU (builds kernel/ first via build.rs)
cargo run

# Run the kernel's unit + integration tests (also boots QEMU, headless,
# and reports results via the isa-debug-exit device)
cd kernel && cargo test
```

Both commands cross-compile against the custom `x86_64-seal_os.json` target using `-Z build-std`, which is why the toolchain must be nightly with `rust-src` installed.

## Testing

Tests run inside the actual kernel environment rather than on the host, since most of this code can't run under a normal OS. Three kinds exist:

- **Unit tests** (`#[test_case]` functions inside `kernel/src/`, e.g. `framebuffer.rs`, `interrupts.rs`) — compiled into the kernel binary itself under `cfg(test)`
- **Integration tests** (`kernel/tests/*.rs`) — each is its own tiny kernel image with its own entry point, boots independently in QEMU:
  - `basic_boot.rs` — smoke test that the kernel reaches framebuffer init and can print
  - `should_panic.rs` — confirms a deliberately failing assertion is correctly detected as a failure
  - `stack_overflow.rs` — deliberately overflows the kernel stack and confirms it's caught as a double fault via the IST-backed handler, rather than triple-faulting the VM

A test binary reports success or failure by writing an exit code to QEMU's `isa-debug-exit` I/O port; `runner` reads QEMU's process exit code and translates it back into something `cargo test` understands.

## Roadmap

Pre-capstone (Layers 1–3, in progress) → capstone (Weeks 1–10): process scheduling, Ring 3 privilege separation, syscalls, a basic shell. Capability-based access control, memory-safe syscalls, and ASLR are stretch goals. See the capstone proposal doc for the full schedule.

## License

MIT — see [LICENSE](LICENSE).
