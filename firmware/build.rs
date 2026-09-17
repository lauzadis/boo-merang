//! Puts `memory.x` where the linker can find it and wires up the RP2040 link
//! scripts. `link-rp.x` comes from embassy-rp and is what places the
//! second-stage bootloader into the BOOT2 region declared in `memory.x` --
//! there is no hand-written `#[link_section = ".boot2"]` anywhere in this crate.

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let out = &PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR always set by cargo"));
    File::create(out.join("memory.x"))
        .expect("create memory.x in OUT_DIR")
        .write_all(include_bytes!("memory.x"))
        .expect("write memory.x");
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");

    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tlink-rp.x");
}
