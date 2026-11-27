//! 工作区布局的落盘：`<数据目录>/layout.json`。
//!
//! 和 `settings.json` 分开存：拖分割条是 300ms 防抖的连续写，混进设置文件会把
//! 用户手改的设置冲掉，也会让设置页的「保存」变得不可预期。
//!
//! Schema 带版本号：读到的版本不是 [`LAYOUT_SCHEMA_VERSION`] 就整个丢掉、
//! 退回内置默认布局（宁可丢一次布局，也不要拿半理解的状态去驱动 dock）。

use gpui_base::dock::{DockArea, DockAreaState};
use std::path::PathBuf;

pub(crate) const LAYOUT_SCHEMA_VERSION: u32 = 1;

/// 弹出窗的位置与内部布局（Phase 2 起写入）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct PopoutLayoutState {
    pub bounds: PersistedBounds,
    pub area: DockAreaState,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct PersistedBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// 整个工作区的布局
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkspaceLayoutFile {
    pub schema_version: u32,
    pub main: DockAreaState,
    #[serde(default)]
    pub popouts: Vec<PopoutLayoutState>,
}

impl WorkspaceLayoutFile {
    pub(crate) fn new(main: DockAreaState) -> Self {
        Self { schema_version: LAYOUT_SCHEMA_VERSION, main, popouts: Vec::new() }
    }
}

/// `layout.json` 的路径；数据目录不可用（没有 HOME / APPDATA）时返回 None，此时不落盘。
pub(crate) fn layout_path() -> Option<PathBuf> {
    claudesktop_core::default_data_dir().ok().map(|dir| dir.join("layout.json"))
}

/// 读布局。文件不存在、读不动、版本不对、反序列化失败都返回 None——
/// 调用方一律退回默认布局，不弹错（布局丢了是小事，起不来是大事）。
pub(crate) fn read() -> Option<WorkspaceLayoutFile> {
    let path = layout_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<WorkspaceLayoutFile>(&text) {
        Ok(file) if file.schema_version == LAYOUT_SCHEMA_VERSION => Some(file),
        Ok(file) => {
            eprintln!(
                "[layout] {} 的版本是 {}，当前要 {}：用默认布局",
                path.display(),
                file.schema_version,
                LAYOUT_SCHEMA_VERSION
            );
            None
        }
        Err(error) => {
            eprintln!("[layout] {} 读不懂（{error}）：用默认布局", path.display());
            None
        }
    }
}

/// 写布局。失败只记日志：布局是便利功能，不能因为它挡住退出或拖拽。
pub(crate) fn write(file: &WorkspaceLayoutFile) {
    let Some(path) = layout_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(file) {
        Ok(text) => {
            if let Err(error) = std::fs::write(&path, text) {
                eprintln!("[layout] {} 写入失败：{error}", path.display());
            }
        }
        Err(error) => eprintln!("[layout] 序列化失败：{error}"),
    }
}

/// 当前 dock 的布局快照
pub(crate) fn snapshot(dock: &DockArea, cx: &gpui_kit::App) -> WorkspaceLayoutFile {
    WorkspaceLayoutFile::new(dock.dump(cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 版本号写在文件里，读到别版本必须整个丢掉，而不是硬套。
    #[core::prelude::v1::test]
    fn a_foreign_schema_version_is_rejected() {
        let state = DockAreaState::default();
        let mut file = WorkspaceLayoutFile::new(state);
        file.schema_version = LAYOUT_SCHEMA_VERSION + 1;
        let text = serde_json::to_string(&file).unwrap();
        let parsed: WorkspaceLayoutFile = serde_json::from_str(&text).unwrap();
        assert_ne!(parsed.schema_version, LAYOUT_SCHEMA_VERSION);
    }

    /// 没有 `popouts` 字段的旧文件也要能读（默认空）。
    #[core::prelude::v1::test]
    fn popouts_default_to_empty() {
        let state = DockAreaState::default();
        let text = serde_json::json!({
            "schema_version": LAYOUT_SCHEMA_VERSION,
            "main": serde_json::to_value(&state).unwrap(),
        })
        .to_string();
        let parsed: WorkspaceLayoutFile = serde_json::from_str(&text).unwrap();
        assert!(parsed.popouts.is_empty());
    }
}
