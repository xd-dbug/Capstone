use std::path::Path;
use std::process::Command;

const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.fd";
const OVMF_VARS: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";

fn main() {
    let uefi_path = env!("UEFI_PATH");

    let vars_copy = Path::new(env!("RUNNER_OUT_DIR")).join("OVMF_VARS.fd");
    if !vars_copy.exists() {
        std::fs::copy(OVMF_VARS, &vars_copy).expect("failed to copy OVMF vars file");
    }

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.arg("-drive")
        .arg(format!("if=pflash,format=raw,readonly=on,file={OVMF_CODE}"));
    cmd.arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", vars_copy.display()));
    cmd.arg("-drive")
        .arg(format!("format=raw,file={uefi_path}"));
    let status = cmd.status().expect("failed to run qemu-system-x86_64");
    std::process::exit(status.code().unwrap_or(1));
}