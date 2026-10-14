//! 色板、字体与字号阶梯（Claude 品牌基准：暖纸底 + 赤陶橙）。

use gpui_kit::{px, relative, DefiniteLength, FontFallbacks, Pixels};

pub const PAPER: u32 = 0xFAF9F5;
pub const SAND: u32 = 0xF0EEE6;
pub const CORAL: u32 = 0xD97757;
pub const SAND_ACTIVE: u32 = 0xE8E3D6;
// 文字色取自原型（更深的墨色）
pub const INK: u32 = 0x1F1E1D;
pub const INK_SOFT: u32 = 0x6E6B65;
pub const INK_FAINT: u32 = 0xB5B0A4;
pub const LINE: u32 = 0xE5E1D5;
pub const CARD: u32 = 0xFFFFFF;
pub const GREEN: u32 = 0x7A9A65;
pub const RED: u32 = 0xC0504D;
/// 实色按钮上的图标 / 文字
pub const WHITE: u32 = 0xFFFFFF;
/// 透明（配 `rgba` 用）
pub const CLEAR: u32 = 0x00000000;
/// 行内代码底色
pub const CODE_BG: u32 = 0xEDE9DF;
/// 用量热力图：0 = 空白格，1..=4 由浅到深的赤陶色
pub const HEAT: [u32; 5] = [0xE8E3D6, 0xF3D9CD, 0xEBB39C, 0xD97757, 0xAE5236];
// 字体：官方的 Styrene 无法分发，用 OFL 近似款，内嵌见 `fonts.rs`。
// 官方 Code 桌面端正文是无衬线，不再用衬线（中文回退成雅黑后会和衬线拉丁字母混排）。
pub const SANS: &str = "Inter";
pub const MONO: &str = "JetBrains Mono";
pub const DIFF_ADD_BG: u32 = 0xE4F3DE;
pub const DIFF_ADD_FG: u32 = 0x3C6B2E;
pub const DIFF_DEL_BG: u32 = 0xFBE3DF;
pub const DIFF_DEL_FG: u32 = 0xA4443A;

/// 中文回退字体（内嵌字体只含拉丁字形）。按顺序取第一个装了的；都没有时交给系统回退。
/// 不定死的话 DirectWrite 会按系统区域挑，可能落到宋体或日文字形。
pub fn cjk_fallbacks() -> FontFallbacks {
    FontFallbacks::from_fonts(
        ["Microsoft YaHei UI", "Microsoft YaHei", "PingFang SC", "Noto Sans CJK SC"]
            .map(String::from)
            .to_vec(),
    )
}

// ---------- 字号阶梯 ----------
// 只用这几档，不要再内联 px 字号。相邻两档至少差 1px，肉眼才分得出层级。

/// 元信息：时间、分组标题、居中备注、状态行
pub const TEXT_META: Pixels = px(12.);
/// 次要文字：说明、提示、chip、diff 头、思考过程正文
pub const TEXT_SMALL: Pixels = px(13.);
/// 界面正文：菜单项、列表标题、工具参数 / 输出、按钮
pub const TEXT_BODY: Pixels = px(14.);
/// 阅读正文：用户消息、助手回复、输入框（也是根视图默认字号）
pub const TEXT_LARGE: Pixels = px(15.);
/// 次级标题：统计数值、回复里的二级标题
pub const TEXT_LEAD: Pixels = px(17.);
/// 回复里的一级标题
pub const TEXT_HEADING: Pixels = px(20.);
/// 页面标题、首页问候
pub const TEXT_TITLE: Pixels = px(28.);

/// 界面多行文字的行高
pub const LEADING: DefiniteLength = relative(1.6);