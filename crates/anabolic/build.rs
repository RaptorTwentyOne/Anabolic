//! Embeds the application manifest and version resource.
//!
//! Release builds ask for `highestAvailable` (admins get the full picture:
//! every process, disk counters, owner SIDs). Debug builds stay `asInvoker`
//! so `cargo run` works from a non-elevated terminal.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Builds the tiny Ctrl+Shift+Esc launcher (its own workspace, its own target
/// dir under OUT_DIR) so anabolic.exe can carry it and drop it next to itself
/// however anabolic itself was built.
fn build_launcher(out: &Path) {
    let crate_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../anabolic-launch");
    println!(
        "cargo:rerun-if-changed={}",
        crate_dir.join("src/main.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        crate_dir.join("Cargo.toml").display()
    );
    let target = out.join("launch-target");
    let status = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--release", "--manifest-path"])
        .arg(crate_dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .expect("could not run cargo for anabolic-launch");
    assert!(status.success(), "building anabolic-launch failed");
    fs::copy(
        target.join("release/anabolic-launch.exe"),
        out.join("anabolic-launch.exe"),
    )
    .unwrap();
}

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    build_launcher(&out);
    println!("cargo:rerun-if-changed=anabolic.exe.manifest");
    println!("cargo:rerun-if-changed=build.rs");

    let level = match env::var("PROFILE").as_deref() {
        Ok("release") => "highestAvailable",
        _ => "asInvoker",
    };
    let manifest = fs::read_to_string("anabolic.exe.manifest")
        .unwrap()
        .replace("$EXECUTION_LEVEL$", level);
    let manifest_path = out.join("anabolic.exe.manifest");
    fs::write(&manifest_path, manifest).unwrap();

    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u16> = version.split('.').filter_map(|p| p.parse().ok()).collect();
    parts.resize(4, 0);
    let numeric = parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");

    let rc = format!(
        r#"#include <winver.h>
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileDescription", "Anabolic - Task Manager on steroids\0"
      VALUE "ProductName", "Anabolic\0"
      VALUE "FileVersion", "{version}\0"
      VALUE "ProductVersion", "{version}\0"
      VALUE "OriginalFilename", "anabolic.exe\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        manifest = manifest_path.display().to_string().replace('\\', "\\\\"),
    );
    let rc_path = out.join("anabolic.rc");
    fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
