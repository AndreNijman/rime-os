//! Embeds res/rime-installer.rc (the manifest and version information) when
//! building for Windows with the GNU toolchain. Without the manifest the
//! installer would start without administrator rights, with the 1990s
//! controls and blurry on a high-DPI screen, so a Windows build without
//! `windres` fails here instead of producing that program.
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=res/rime-installer.rc");
    println!("cargo:rerun-if-changed=res/rime-installer.manifest");
    println!("cargo:rerun-if-env-changed=RIME_ISO_URL");
    println!("cargo:rerun-if-env-changed=RIME_ISO_SHA256");
    println!("cargo:rerun-if-env-changed=RIME_ISO_BYTES");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        panic!("only the x86_64-pc-windows-gnu target embeds the manifest; see build-windows.sh");
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("rime-installer-res.o");
    let windres = env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let ok = Command::new(&windres)
        .args(["--input-format=rc", "--output-format=coff", "-I", "res", "-i", "res/rime-installer.rc", "-o"])
        .arg(&out)
        .status()
        .unwrap_or_else(|e| panic!("{windres} could not be run ({e}); install mingw64-binutils"));
    assert!(ok.success(), "{windres} failed");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}
