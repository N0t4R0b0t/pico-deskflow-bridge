use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // The board feature picks the memory layout; put it where the linker
    // finds it as memory.x.
    let rp2350 = env::var_os("CARGO_FEATURE_PICO2_W").is_some();
    let memory = if rp2350 { "memory-rp2350.x" } else { "memory-rp2040.x" };
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::copy(memory, out.join("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory-rp2040.x");
    println!("cargo:rerun-if-changed=memory-rp2350.x");

    // Shown on the settings page, so it's clear what's running.
    let version = Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok());
    println!("cargo:rustc-env=BUILD_ID={}", version.as_deref().unwrap_or("unknown").trim());
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/index");

    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    // The RP2040's boot2 placement; the RP2350 boots from the Image
    // Definition in memory-rp2350.x instead.
    if !rp2350 {
        println!("cargo:rustc-link-arg-bins=-Tlink-rp.x");
    }
}
