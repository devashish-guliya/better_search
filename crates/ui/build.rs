//! Embeds `bs-window.manifest` into the executable. The manifest activates the
//! common controls v6 and declares per-monitor DPI awareness. This uses only the
//! MSVC linker (already required to build), so there is no new dependency or runtime.

use std::path::Path;

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("bs-window.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    // `/MANIFESTINPUT` requires `/MANIFEST:EMBED`; both are passed straight to link.exe.
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
