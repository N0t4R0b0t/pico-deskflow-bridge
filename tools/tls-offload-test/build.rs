//! Builds firmware/src/tls_offload.rs for the host: the module itself is
//! unchanged apart from the hardware it touches (clock, atomics, the task
//! attribute), so the test always exercises the current firmware code.

use std::{env, fs, path::PathBuf};

fn main() {
    let src = "../../firmware/src/tls_offload.rs";
    println!("cargo:rerun-if-changed={src}");
    let code = fs::read_to_string(src).unwrap();
    let mut out = String::new();
    for line in code.lines() {
        let line = match line.trim() {
            // include!() inside a module can't take inner doc comments.
            l if l.starts_with("//!") => continue,
            "#[embassy_executor::task]" => continue,
            "use embassy_time::Instant;" => "use crate::shim::Instant;",
            "use portable_atomic::{AtomicBool, AtomicU32, Ordering};" => "use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};",
            _ => line,
        };
        out.push_str(line);
        out.push('\n');
    }
    for expected in ["crate::shim::Instant", "core::sync::atomic"] {
        assert!(out.contains(expected), "tls_offload.rs changed; update tools/tls-offload-test/build.rs ({expected})");
    }
    let dest = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("tls_offload.rs");
    fs::write(dest, out).unwrap();
}
