//! Put link.x on the linker search path (so `-Tlink.x` resolves).
use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::copy("link.x", out.join("link.x")).expect("copy link.x");
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=link.x");
}
