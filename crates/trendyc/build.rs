//! Build the runtime as a static library, so that `trendyc` can embed it and
//! link it into every executable.
//!
//! The runtime is an ordinary workspace crate, built here a second time with
//! `cargo rustc --crate-type staticlib` and `panic = "abort"`, which a
//! `no_std` static library needs. It gets a target directory of its own, so
//! this nested build never waits on the outer build's lock.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let workspace = manifest_dir.join("../..");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo"));
    let target_dir = out_dir.join("runtime-target");
    let cargo = env::var_os("CARGO").expect("set by cargo");

    let status = Command::new(cargo)
        .current_dir(&workspace)
        .args([
            "rustc",
            "--quiet",
            "--locked",
            "--offline",
            "--release",
            "--package",
            "trendy_runtime",
            "--crate-type",
            "staticlib",
            "--config",
            "profile.release.panic=\"abort\"",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("running cargo to build the runtime");
    assert!(status.success(), "building the runtime failed: {status}");

    let archive = target_dir.join("release/libtrendy_runtime.a");
    std::fs::copy(&archive, out_dir.join("libtrendy_runtime.a"))
        .expect("copying the runtime archive");

    for path in ["crates/runtime", "crates/text"] {
        println!("cargo::rerun-if-changed={}", workspace.join(path).display());
    }
}
