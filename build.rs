//! Compiles the Windows resources: icon, manifest and version info.
//!
//! The resource script is generated, so the version info always matches the
//! version in `Cargo.toml`. Code signing checks the product name and version.

use std::env;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=res/widepeek.manifest");
    println!("cargo:rerun-if-changed=res/widepeek.ico");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let rc_path = out_dir.join("widepeek.rc");
    std::fs::write(&rc_path, resource_script(&manifest_dir.join("res")))
        .expect("failed to write the resource script");

    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .expect("failed to compile Windows resources");
}

fn resource_script(res_dir: &Path) -> String {
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let numeric = ["MAJOR", "MINOR", "PATCH"]
        .map(|part| env::var(format!("CARGO_PKG_VERSION_{part}")).expect("version part"))
        .join(",");
    let description = env::var("CARGO_PKG_DESCRIPTION").expect("CARGO_PKG_DESCRIPTION");
    let icon = rc_string(&res_dir.join("widepeek.ico"));
    let manifest = rc_string(&res_dir.join("widepeek.manifest"));

    let mut rc = String::new();
    writeln!(rc, "#pragma code_page(65001)").unwrap();
    writeln!(rc, "1 ICON \"{icon}\"").unwrap();
    writeln!(rc, "1 24 \"{manifest}\"").unwrap();
    write!(
        rc,
        r#"
1 VERSIONINFO
FILEVERSION {numeric},0
PRODUCTVERSION {numeric},0
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "widepeek"
      VALUE "LegalCopyright", "Copyright (c) 2026 widepeek contributors. MIT License."
      VALUE "OriginalFilename", "widepeek.exe"
      VALUE "ProductName", "widepeek"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
    .unwrap();
    rc
}

/// A path as a resource-script string literal body.
fn rc_string(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}
