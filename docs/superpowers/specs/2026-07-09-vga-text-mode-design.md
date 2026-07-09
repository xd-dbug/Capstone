# VGA-style Text Output Design

## Context

`seal_os` boots via the `bootloader` crate (0.11.15, UEFI feature only). Since that
crate's post-0.10 rewrite, UEFI boot hands the kernel a linear pixel (GOP)
framebuffer through `BootInfo`, not a legacy `0xb8000` VGA text-mode buffer.
Real hardware VGA text mode does not exist as an option under this bootloader
configuration, UEFI or not.

The kernel currently only proves boot works by filling the framebuffer solid
green (`kernel/src/main.rs`). `src/vga_buffer.rs` is a dead stub (just a
`Color` enum and `ColorCode`) sitting in the `runner` crate — a `std` host
binary that only launches QEMU and never runs as the kernel, so it can't host
this code at all.

There is also a staged, incomplete `kernel/src/vga_buffer.rs` (correct crate,
but a different approach): it follows the tutorial's original/first-edition
chapter, writing directly to raw address `0xb8000` via an unsafe pointer cast.
This predates realizing that `0xb8000` isn't the VGA text buffer under this
project's UEFI/GOP-framebuffer boot — writing there wouldn't render anything.
It is leftover exploration and is superseded by this design; it should be
removed (along with the stray `mod vga_buffer;` line added to the *runner*
crate's `src/main.rs`, which doesn't even resolve to this file) rather than
built on top of.

Goal: be able to print text (starting with `Hello World!`) to the screen from
kernel code, and see kernel panics on screen instead of a silent infinite
loop. No requirement (course, rubric, or otherwise) mandates literal `0xb8000`
access — a software-rendered text console on top of the existing pixel
framebuffer satisfies the need.

This mirrors what the current edition of "Writing an OS in Rust" does at this
exact point in its own kernel — the green-fill code already in
`kernel/src/main.rs` is from that same chapter, immediately before it adds
text rendering.

## Architecture & File Layout

The feature lives entirely in the `kernel` crate, not `runner`. Delete
`src/vga_buffer.rs` from the runner crate (unused dead code in the wrong
crate), remove the staged `kernel/src/vga_buffer.rs` (superseded 0xb8000
exploration, see Context), and remove the stray `mod vga_buffer;` line from
the runner crate's `src/main.rs`.

```
kernel/
  Cargo.toml          (+ noto-sans-mono-bitmap, + spin/conquer_once)
  src/
    main.rs            kernel_main initializes the writer, then println!s
    framebuffer.rs      Writer, glyph blitting, print!/println! macros, Write impl
```

Unlike real VGA text mode, where `0xb8000` is a fixed compile-time address,
this framebuffer's pointer, resolution, and pixel format are only known at
runtime, from `boot_info.framebuffer` inside `kernel_main`. The global writer
therefore can't be a `static` initialized at compile time (no `lazy_static`
"first access" hook applies). It must be a `static` holding an empty cell
(`conquer_once::spin::OnceCell`, or `spin::Mutex<Option<Writer>>`) that
`kernel_main` fills in once, right after it obtains `boot_info`.

## Components

**`framebuffer.rs`**

- `struct Writer` — owns the raw framebuffer byte slice, `FrameBufferInfo`
  (resolution, stride, pixel format from `bootloader_api`), and an x/y pixel
  cursor.
- `impl Writer`:
  - `write_char(c: char)` — looks up the glyph via
    `noto_sans_mono_bitmap::get_raster`, blits its pixels at the cursor
    position (accounting for the framebuffer's pixel format — RGB vs BGR,
    since `bootloader_api` doesn't guarantee one), then advances the cursor
    by the glyph width.
  - Newline handling: `\n` moves the cursor to `x = 0`, `y += glyph_height`.
    If `y` would run past the bottom of the screen, wrap back to `y = 0` and
    clear the framebuffer (no real scrolling in this pass — matches the
    blog's minimal version).
- `impl core::fmt::Write for Writer` — `write_str` calls `write_char` per
  character, which is what makes `write!`/`writeln!` (and thus `println!`)
  work.
- `static WRITER: OnceCell<spin::Mutex<Writer>>` plus
  `init(framebuffer: &'static mut FrameBuffer)`, called once from
  `kernel_main`.
- `print!`/`println!` macros (same shape as `std`'s) that lock `WRITER` and
  call `write_fmt`. If `WRITER` isn't initialized yet, they no-op rather than
  panicking.

**`main.rs`** — `kernel_main` calls `framebuffer::init(...)` on the boot-info
framebuffer, then `println!("Hello World!")`.

**Panic handler** — `#[panic_handler]` calls
`println!("KERNEL PANIC: {}", info)` (no-op if the writer was never
initialized) before looping forever.

## Data Flow

`bootloader` → UEFI boots kernel with `BootInfo` → `kernel_main` takes
`boot_info.framebuffer`, passes it to `framebuffer::init` → `WRITER` is
populated → any code (including the panic handler) can call
`println!`/`print!` from then on → each call locks the mutex, writes chars
via glyph blitting into the raw framebuffer bytes, which QEMU displays live.

## Error Handling

Kept minimal, matching the "just print text" scope:

- Glyph not found for a `char` (e.g. unsupported unicode) → fall back to
  printing `'?'` rather than panicking, since `noto-sans-mono-bitmap` only
  covers a fixed character set.
- `println!` called before `framebuffer::init` → silently no-ops.
- Text running off the bottom of the screen → clear and wrap to top; the
  writer computes its own bounds from `FrameBufferInfo`, so it can't overrun
  the buffer.
- No framebuffer at all → unchanged from today: `kernel_main` already does
  `if let Some(framebuffer) = ...`; if there's no framebuffer, there's
  nowhere to print, and the kernel just loops as it does now.

## Testing

No unit tests — this is visual, hardware-adjacent code. Verification is
running the existing `cargo run` (root `runner` crate → builds kernel → boots
via OVMF/QEMU) and confirming `Hello World!` renders as text in the QEMU
window. Additionally, verify the panic path by temporarily forcing a
`panic!()` in `kernel_main` and confirming the message appears on screen
instead of a silent hang.

## Explicitly Out of Scope

- Real scrolling (lines shift up instead of wrap/clear).
- A fixed 16-color VGA-style palette — this renderer can use arbitrary RGB,
  so a hardware-style palette isn't a hard constraint; add one later only if
  wanted for aesthetics.
- Real legacy `0xb8000` VGA text mode — not available under the current
  `bootloader` 0.11 UEFI configuration; would require a different boot
  pipeline entirely (see Context).