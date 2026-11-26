//! 运行状态语义。
//!
//! 界面暂不放置品牌 Logo；左上角使用文字标题。运行状态使用线性图标或统一的弹性双块。

/// 运行状态行的语义，由 `Activity` 和准备状态映射到极简几何指示器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mood {
    Think,
    Talk,
    Ask,
    Sweat,
    Squish,
    Wake,
    Scan,
}
