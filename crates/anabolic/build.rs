//! Embeds the application manifest and version resource.
//!
//! Release builds ask for `highestAvailable` (admins get the full picture:
//! every process, disk counters, owner SIDs). Debug builds stay `asInvoker`
//! so `cargo run` works from a non-elevated terminal.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Linux has no resources: the icon comes with the .desktop file.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=anabolic.exe.manifest");
    println!("cargo:rerun-if-changed=assets/logo/anabolic.ico");

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
1 ICON "{icon}"
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
      VALUE "FileDescription", "Anabolic - performance monitoring and diagnostics\0"
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
        icon = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("assets/logo/anabolic.ico")
            .display()
            .to_string()
            .replace('\\', "\\\\"),
    );
    let rc_path = out.join("anabolic.rc");
    fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
