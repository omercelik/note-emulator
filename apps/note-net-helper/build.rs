use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let sdk = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .expect("xcrun");
    if !sdk.status.success() {
        panic!("xcrun --show-sdk-path failed: {}", String::from_utf8_lossy(&sdk.stderr));
    }
    let sdk = String::from_utf8(sdk.stdout).expect("sdk path").trim().to_string();
    let obj = out_dir.join("vmnet_shim.o");
    let status = Command::new("clang")
        .args(["-c", "-fblocks", "-fPIC", "-O2", "-Wall", "-Werror", "-isysroot"])
        .arg(&sdk)
        .arg("-o")
        .arg(&obj)
        .arg("src/vmnet_shim.c")
        .status()
        .expect("clang");
    if !status.success() {
        panic!("clang failed to compile src/vmnet_shim.c");
    }
    let archive = out_dir.join("libvmnet_shim.a");
    let status = Command::new("ar").args(["rcs"]).arg(&archive).arg(&obj).status().expect("ar");
    if !status.success() {
        panic!("ar failed to archive vmnet_shim");
    }
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=vmnet_shim");
    println!("cargo:rustc-link-lib=framework=vmnet");
    println!("cargo:rerun-if-changed=src/vmnet_shim.c");
    println!("cargo:rerun-if-changed=build.rs");
}
