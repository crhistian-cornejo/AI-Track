//! Windows only: gives `aitrack.exe` its icon and version resource (Explorer's Details
//! tab, the taskbar and the installer all read them). Other targets do nothing.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../release/icons/aitrack.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        windows_resource();
    }
}

#[cfg(windows)]
fn windows_resource() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut parts = version.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse::<u16>().ok());
    let (major, minor, patch) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    let icon = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../release/icons/aitrack.ico");
    let icon = icon.canonicalize().unwrap_or(icon).display().to_string().replace('\\', "/");
    let icon = icon.strip_prefix("//?/").unwrap_or(&icon);
    // Language 0409 (US English), code page 04b0 (Unicode).
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004
FILETYPE 1
{{
  BLOCK "StringFileInfo"
  {{
    BLOCK "040904b0"
    {{
      VALUE "ProductName", "AI-Track"
      VALUE "FileDescription", "AI-Track"
      VALUE "InternalName", "aitrack"
      VALUE "OriginalFilename", "aitrack.exe"
      VALUE "LegalCopyright", "MIT License"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
    }}
  }}
  BLOCK "VarFileInfo"
  {{
    VALUE "Translation", 0x409, 0x4b0
  }}
}}
"#
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("aitrack.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(out, embed_resource::NONE).manifest_optional().unwrap();
}
