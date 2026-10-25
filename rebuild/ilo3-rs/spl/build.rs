//! Put link-spl.x on the linker search path and select it (XIP @ 0xFFFF0000).
use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::copy("link-spl.x", out.join("link-spl.x")).expect("copy link-spl.x");
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rustc-link-arg=-Tlink-spl.x");
    println!("cargo:rerun-if-changed=link-spl.x");
}
