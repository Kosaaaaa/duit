//! Links the `duit` binary with `duit.order` on Apple targets.
//!
//! The order file lists the functions that the streaming paths run, so that the Apple linker lays
//! them out next to each other and these paths keep fewer pages of code resident. The linker skips
//! the names that it does not find, so a build whose symbol names differ, with another toolchain
//! or other versions of the crates, links as without the file. `scripts/order_file.py` writes it.

use std::env;
use std::path::Path;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=duit.order");
    if env::var_os("CARGO_CFG_TARGET_VENDOR").is_some_and(|vendor| vendor == "apple")
        && let Some(manifest) = env::var_os("CARGO_MANIFEST_DIR")
    {
        let order = Path::new(&manifest).join("duit.order");
        // `-Xlinker` passes the path as its own argument, which `-Wl,` would split at a comma.
        println!("cargo::rustc-link-arg-bin=duit=-Xlinker");
        println!("cargo::rustc-link-arg-bin=duit=-order_file");
        println!("cargo::rustc-link-arg-bin=duit=-Xlinker");
        println!("cargo::rustc-link-arg-bin=duit={}", order.display());
    }
}
