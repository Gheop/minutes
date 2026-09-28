//! Compiles the translations next to the build, so a binary run from
//! `target/` finds them before they are installed, and tells the binary where
//! the libraries shipped with it are.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // The Vulkan build finds the speakers through ONNX Runtime's WebGPU,
    // whose library (Dawn) comes with it: next to the binary as built,
    // in ~/.local/lib/minutes as installed.
    println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/minutes");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("locale");
    println!("cargo:rerun-if-changed=po");
    for lang in std::fs::read_to_string("po/LINGUAS")
        .unwrap_or_default()
        .split_whitespace()
    {
        let dir = out.join(lang).join("LC_MESSAGES");
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new("msgfmt")
            .arg("-o")
            .arg(dir.join("minutes.mo"))
            .arg(format!("po/{lang}.po"))
            .status();
        if !status.is_ok_and(|s| s.success()) {
            println!("cargo:warning=msgfmt failed for po/{lang}.po; that translation is left out");
        }
    }
}
