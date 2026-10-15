//! 色板、字体与字号阶梯（Claude 品牌基准：暖纸底 + 赤陶橙）。

use gpui_kit::{DefiniteLength, FontFallbacks, FontFeatures, Pixels, Rgba, px, relative};
use std::sync::atomic::{AtomicBool, Ordering};

// ---------- 色板 ----------
// 每个颜色是浅色 / 深色一对，界面里一律写 `rgb(INK)`：这里的 `rgb` 遮住了 gpui 的同名函数，
// 按当前外观取值（各文件要 `use crate::theme::rgb;`，否则和 `gpui_kit::*` 里的重名）。
// 直接传十六进制会编译不过——新颜色先加到这里，浅色 / 深色都给值，并校对对比度。
// 深色基于 claude.ai 的暖炭灰（不是纯黑），赤陶橙两边同色。

/// 一个语义颜色的浅色 / 深色取值（`0xRRGGBB`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub light: u32,
    pub dark: u32,
}

const fn c(light: u32, dark: u32) -> Color {
    Color { light, dark }
}

static DARK: AtomicBool = AtomicBool::new(false);

/// 切换外观（由根视图按设置 / 系统外观调用，之后要重绘）。
pub fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// 按当前外观取颜色。
pub fn rgb(color: Color) -> Rgba {
    gpui_kit::rgb(if is_dark() { color.dark } else { color.light })
}

/// 主区底色（纸）
pub const PAPER: Color = c(0xFAF9F5, 0x262624);
/// 凹下去的面：侧栏、用户气泡、代码块、概览面板、chip（深色下比主区更暗）
pub const SAND: Color = c(0xF0EEE6, 0x1F1E1D);
pub const SAND_ACTIVE: Color = c(0xE8E3D6, 0x33322F);
/// 浮起来的面：输入框、卡片、弹层
pub const CARD: Color = c(0xFFFFFF, 0x30302E);
pub const CORAL: Color = c(0xD97757, 0xD97757);
/// 带文字的主按钮底色：与 CORAL 同色相、压暗一档（白字 4.8:1）。
/// CORAL 上的白字只有 3.1:1，只够图标用（如发送按钮）
pub const CORAL_DEEP: Color = c(0xBF502B, 0xBF502B);
/// 带文字的主按钮悬停（白字 5.5:1）
pub const CORAL_DEEP_HOVER: Color = c(0xAF4928, 0xAF4928);
// 文字色取自原型（更深的墨色）
pub const INK: Color = c(0x1F1E1D, 0xF5F4EE);
/// 次要文字：说明、元信息、时间（浅色：纸底 5.0:1、沙色底 4.6:1；深色：各底 ≥ 5.9:1）
pub const INK_SOFT: Color = c(0x6E6B65, 0xBDB9AE);
/// 只给占位、禁用、分隔符和装饰用（纸底上约 2–3:1）——承载信息的文字用 `INK_SOFT`
pub const INK_FAINT: Color = c(0xB5B0A4, 0x6E6C66);
pub const LINE: Color = c(0xE5E1D5, 0x3D3C38);
/// 悬停时加深一档的边框（输入框），介于 `LINE` 和聚焦用的 `INK_FAINT` 之间
pub const LINE_STRONG: Color = c(0xD3CEC1, 0x55534E);
pub const GREEN: Color = c(0x7A9A65, 0x94B07E);
/// 红色文字 / 小圆点（深色下提亮，只适合放在底色上当前景）
pub const RED: Color = c(0xC0504D, 0xE5726C);
/// 危险操作的实色按钮底（白字 4.7 / 5.4:1）：删除确认、窗口关闭悬停
pub const DANGER: Color = c(0xC0504D, 0xB8433F);
/// 实色按钮上的图标 / 文字（两种外观都是白）
pub const WHITE: Color = c(0xFFFFFF, 0xFFFFFF);
/// 透明（配 gpui 的 `rgba` 用，与外观无关）
pub const CLEAR: u32 = 0x00000000;
/// 行内代码底色
pub const CODE_BG: Color = c(0xEDE9DF, 0x1A1918);
/// 用量热力图：0 = 空白格，1..=4 由浅到深（深色下由暗到亮）的赤陶色
pub const HEAT: [Color; 5] = [
    SAND_ACTIVE,
    c(0xF3D9CD, 0x4A3329),
    c(0xEBB39C, 0x7A4632),
    c(0xD97757, 0xB85F42),
    c(0xAE5236, 0xE3866A),
];

// ---------- 交互态 ----------
// 同一种底上的悬停永远是同一个颜色；不要再用 LINE / PAPER / 透明度充当悬停。

/// 纸底 / 卡片上的悬停
pub const HOVER: Color = c(0xF0EEE6, 0x3A3936);
/// 选中态，以及沙色底（侧栏、概览面板）上的悬停
pub const SELECTED: Color = SAND_ACTIVE;
/// 已经是 `SELECTED` 的行里再叠一层的悬停（侧栏会话行上的小图标按钮）
pub const HOVER_STRONG: Color = c(0xDCD6C6, 0x45443F);
// 字体：官方的 Styrene 无法分发，用 OFL 近似款，内嵌见 `fonts.rs`。
// 官方 Code 桌面端正文是无衬线，不再用衬线（中文回退成雅黑后会和衬线拉丁字母混排）。
pub const SANS: &str = "Inter";
pub const MONO: &str = "JetBrains Mono";
pub const DIFF_ADD_BG: Color = c(0xE4F3DE, 0x243222);
pub const DIFF_ADD_FG: Color = c(0x3C6B2E, 0x9FCC8C);
pub const DIFF_DEL_BG: Color = c(0xFBE3DF, 0x3A2422);
pub const DIFF_DEL_FG: Color = c(0xA4443A, 0xF19B90);

/// 中文回退字体（内嵌字体只含拉丁字形）。按顺序取第一个装了的；都没有时交给系统回退。
/// 不定死的话 DirectWrite 会按系统区域挑，可能落到宋体或日文字形。
pub fn cjk_fallbacks() -> FontFallbacks {
    FontFallbacks::from_fonts(
        ["Microsoft YaHei UI", "Microsoft YaHei", "PingFang SC", "Noto Sans CJK SC"]
            .map(String::from)
            .to_vec(),
    )
}

/// 等宽数字（OpenType `tnum`）：用时、token 这类会跳动的数字用它，位数不变时宽度不变，
/// 后面的文字不会跟着左右抖。只影响数字字形，文字照常。
pub fn tabular_nums() -> FontFeatures {
    FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))
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

// ---------- 圆角 ----------
// 只用 gpui 的档位，不要写 `rounded(px(..))`。随尺寸递增，嵌套时外圆角 = 内圆角 + 内边距：
// rounded_xs  2px  停止按钮里的小方块
// rounded_sm  4px  行内代码、参数药丸、热力格
// rounded_md  6px  chip、菜单项、图标按钮
// rounded_lg  8px  按钮、缩略图、发送按钮
// rounded_xl  12px 卡片、弹层、权限卡、侧栏面板
// rounded_2xl 16px 输入框、用户气泡

// ---------- 布局宽度 ----------

/// 对话列与输入框的最大宽度（阅读列）
pub const READING_W: Pixels = px(760.);
/// 设置页内容最大宽度
pub const FORM_W: Pixels = px(720.);
/// 设置页左侧分类导航宽度
pub const SETTINGS_NAV_W: Pixels = px(200.);
/// 首页用量概览面板宽度
pub const OVERVIEW_W: Pixels = px(724.);
/// 侧栏宽度（含外圈留白）
pub const SIDEBAR_W: Pixels = px(256.);

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn rgb_follows_appearance() {
        set_dark(false);
        assert_eq!(rgb(PAPER), gpui_kit::rgb(0xFAF9F5));
        set_dark(true);
        assert_eq!(rgb(PAPER), gpui_kit::rgb(0x262624));
        // 赤陶橙两边同色
        assert_eq!(rgb(CORAL), gpui_kit::rgb(0xD97757));
        set_dark(false);
    }
}
