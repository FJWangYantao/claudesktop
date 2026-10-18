//! Windows：把图标和版本信息编进 exe。图标必须是 1 号资源，GPUI 建窗口时按这个编号取
//! （任务栏、标题栏、资源管理器里看到的都是它）。版本号跟 Cargo.toml 走，所以 .rc 在这里生成。

fn main() {
    println!("cargo:rerun-if-changed=resources/claudesktop.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u16> =
        version.split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect();
    parts.resize(4, 0);
    let num = parts.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let icon = format!("{manifest}/resources/claudesktop.ico").replace('\\', "/");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {num}
PRODUCTVERSION {num}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "claudesktop"
      VALUE "ProductName", "claudesktop"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "claudesktop.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("claudesktop.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}
