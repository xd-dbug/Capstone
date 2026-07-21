use std::env;
use std::path::PathBuf;
use std::process::Command;

const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.fd";
const OVMF_VARS: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";

/// QEMU's isa-debug-exit device turns a written value `v` into exit code `(v << 1) | 1`.
const QEMU_TEST_SUCCESS: i32 = 33; // (0x10 << 1) | 1, see kernel::QemuExitCode::Success

/// Custom `cargo run`/`cargo test` runner: boots the kernel (or a test binary)
/// in QEMU via UEFI, translating the VM's exit code to a cargo-friendly one.
fn main() {
    // Cargo invokes a configured `runner` as `<runner> <path-to-executable>`. When we're
    // run that way (from kernel/.cargo/config.toml), argv[1] is the kernel or test binary
    // to boot. When we're run directly via `cargo run` at the repo root, there's no argv[1]
    // and we fall back to the kernel ELF that our own build.rs just cross-compiled.
    let mut args = env::args().skip(1);
    let kernel_elf = match args.next() {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("KERNEL_ELF")),
    };

    // Test binaries (unit test harnesses and integration tests) are always placed under a
    // `deps/` directory by cargo; the normal kernel binary is not. We use that to decide
    // whether to wire up the QEMU test-exit device and run headless.
    let is_test = kernel_elf
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|name| name == "deps");

    let out_dir = PathBuf::from(env!("RUNNER_OUT_DIR"));

    let mut boot_config = bootloader::BootConfig::default();
    boot_config.frame_buffer_logging = false;

    let image_name = format!(
        "uefi-{}.img",
        kernel_elf.file_name().unwrap().to_string_lossy()
    );
    let uefi_path = out_dir.join(image_name);
    bootloader::UefiBoot::new(&kernel_elf)
        .set_boot_config(&boot_config)
        .create_disk_image(&uefi_path)
        .expect("failed to create UEFI disk image");

    let vars_copy = out_dir.join("OVMF_VARS.fd");
    if !vars_copy.exists() {
        std::fs::copy(OVMF_VARS, &vars_copy).expect("failed to copy OVMF vars file");
    }

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.arg("-drive")
        .arg(format!("if=pflash,format=raw,readonly=on,file={OVMF_CODE}"));
    cmd.arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", vars_copy.display()));
    cmd.arg("-drive")
        .arg(format!("format=raw,file={}", uefi_path.display()));

    if is_test {
        cmd.arg("-device")
            .arg("isa-debug-exit,iobase=0xf4,iosize=0x04");
        cmd.arg("-serial").arg("stdio");
        cmd.arg("-display").arg("none");
    }

    let status = cmd.status().expect("failed to run qemu-system-x86_64");
    let code = status.code().unwrap_or(1);

    if is_test {
        // Map QEMU's isa-debug-exit code back to a process exit code cargo understands:
        // 0 means the test binary reported success.
        std::process::exit(if code == QEMU_TEST_SUCCESS { 0 } else { 1 });
    } else {
        std::process::exit(code);
    }
}