use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let kernel_dir = manifest_dir.join("kernel");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let profile = env::var("PROFILE").unwrap();

    println!("cargo:rerun-if-changed={}", kernel_dir.join("src").display());
    println!("cargo:rerun-if-changed={}", kernel_dir.join("Cargo.toml").display());

    let mut cmd = Command::new(env::var_os("CARGO").unwrap());
    cmd.current_dir(&kernel_dir);
    cmd.arg("build");
    if profile == "release" {
        cmd.arg("--release");
    }
    let status = cmd.status().expect("failed to run cargo to build kernel");
    if !status.success() {
        panic!("kernel build failed");
    }

    let kernel_target_dir = kernel_dir.join("target/x86_64-seal_os").join(&profile);
    let kernel_elf = kernel_target_dir.join("kernel");
    assert!(
        kernel_elf.exists(),
        "expected kernel binary at {}",
        kernel_elf.display()
    );


    let uefi_path = out_dir.join("uefi.img");
    bootloader::UefiBoot::new(&kernel_elf)
        .create_disk_image(&uefi_path)
        .expect("failed to create UEFI disk image");

    println!("cargo:rustc-env=UEFI_PATH={}", uefi_path.display());
    println!("cargo:rustc-env=RUNNER_OUT_DIR={}", out_dir.display());
}