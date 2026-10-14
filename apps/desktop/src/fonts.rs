//! 内嵌字体：Inter（界面与正文）、JetBrains Mono（代码 / 工具行）。均为 SIL OFL 1.1，来源见 `assets/fonts/README.md`。
//!
//! 只含拉丁字形；中文由系统字体回退（DirectWrite fallback）。

use gpui_kit::App;
use std::borrow::Cow;

macro_rules! font {
    ($file:literal) => {
        Cow::Borrowed(include_bytes!(concat!("../assets/fonts/", $file)).as_slice())
    };
}

/// 启动时注册一次；失败只打日志，界面会退回系统字体。
pub fn register(cx: &mut App) {
    let fonts = vec![
        font!("Inter-400-normal.ttf"),
        font!("Inter-500-normal.ttf"),
        font!("Inter-600-normal.ttf"),
        font!("JetBrainsMono-400-normal.ttf"),
        font!("JetBrainsMono-500-normal.ttf"),
    ];
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("[claudesktop] 内嵌字体注册失败，退回系统字体: {err:#}");
    }
}
