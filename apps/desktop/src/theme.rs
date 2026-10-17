//! 色板、字体与字号阶梯（Claude 品牌基准：暖纸底 + 赤陶橙）。

use gpui_kit::{
    App, DefiniteLength, FontFallbacks, FontFeatures, Pixels, Rems, Rgba, SharedString, px,
    relative, rems,
};
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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
/// 默认界面字体族（`fonts.rs` 内嵌的 Inter）
pub const DEFAULT_SANS: &str = "Inter";
/// 代码 / 工具行的等宽字体（`fonts.rs` 内嵌；不随设置变化，代码靠它对齐）
pub const MONO: &str = "JetBrains Mono";
/// 符号字形（等待动效的 ✻ 系字形）：不指定的话 ✳ 会回退到彩色 emoji 字体，画成绿方块
pub const SYMBOL: &str = "Segoe UI Symbol";
pub const DIFF_ADD_BG: Color = c(0xE4F3DE, 0x243222);
pub const DIFF_ADD_FG: Color = c(0x3C6B2E, 0x9FCC8C);
pub const DIFF_DEL_BG: Color = c(0xFBE3DF, 0x3A2422);
pub const DIFF_DEL_FG: Color = c(0xA4443A, 0xF19B90);
/// 文字选区底色（输入框、Markdown）：淡赤陶（浅色上墨色字 12:1，深色上 8.9:1）
pub const SELECTION: Color = c(0xF3D9CD, 0x5C3B2E);

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
// 只用这几档，不要再内联字号。相邻两档至少差 1px，肉眼才分得出层级。
//
// 单位是 rem 而不是 px：字号、间距、圆角、图标尺寸全都按窗口的 rem 换算，
// 所以只要改根字号（见下面的 `set_font_scale` / `apply`），整套界面等比缩放。
// 1rem = 16 × 缩放倍率，这里的数值就是「标准档下多少 px / 16」。

/// 元信息：时间、分组标题、居中备注、状态行（标准档 12px）
pub const TEXT_META: Rems = rems(0.75);
/// 次要文字：说明、提示、chip、diff 头、思考过程正文（13px）
pub const TEXT_SMALL: Rems = rems(0.8125);
/// 界面正文：菜单项、列表标题、工具参数 / 输出、按钮（14px）
pub const TEXT_BODY: Rems = rems(0.875);
/// 阅读正文：用户消息、助手回复、输入框（15px，也是根视图默认字号）
pub const TEXT_LARGE: Rems = rems(0.9375);
/// 次级标题：统计数值、回复里的二级标题（17px）
pub const TEXT_LEAD: Rems = rems(1.0625);
/// 回复里的一级标题（20px）
pub const TEXT_HEADING: Rems = rems(1.25);
/// 页面标题、首页问候（28px）
pub const TEXT_TITLE: Rems = rems(1.75);

/// 界面多行文字的行高
pub const LEADING: DefiniteLength = relative(1.6);

// ---------- 字体设置（设置页可调） ----------
// 字号只有一个开关：根 em 值。gpui 的间距 / 圆角 / 图标 / 尺寸 helper 全是 rem，
// 而 `gpui-component` 的 Root 每帧把 `Theme::font_size` 写进 `window.set_rem_size()`，
// 所以改这一个值，整套界面等比缩放。字体族只影响界面文字，代码行固定用 MONO。

/// 标准档的根字号（1rem = 16px 时的界面基准）。
const BASE_FONT_SIZE: f32 = 16.;
/// 缩放倍率的上下限（设置页滑杆用百分比表示：85% ~ 140%）。
pub const FONT_SCALE_MIN: f32 = 0.85;
pub const FONT_SCALE_MAX: f32 = 1.4;
/// 滑杆步进（5%）与默认档（100%）。
pub const FONT_SCALE_STEP: f32 = 5.;
pub const FONT_SCALE_DEFAULT: f32 = 1.0;

/// 缩放倍率（存千分比整数，避免浮点原子性问题）。
static FONT_SCALE_MILLI: AtomicU32 = AtomicU32::new((FONT_SCALE_DEFAULT * 1000.) as u32);
/// 界面字体族；None = 内嵌的 Inter。
static FONT_FAMILY: RwLock<Option<SharedString>> = RwLock::new(None);
/// 系统已装字体族（进程内只枚举一次）。
static INSTALLED_FONTS: OnceLock<Vec<String>> = OnceLock::new();

/// 设置界面缩放（倍率，1.0 = 标准）；超范围自动夹住。
pub fn set_font_scale(scale: f32) {
    let clamped = scale.clamp(FONT_SCALE_MIN, FONT_SCALE_MAX);
    FONT_SCALE_MILLI.store((clamped * 1000.).round() as u32, Ordering::Relaxed);
}

/// 当前缩放（倍率，1.0 = 标准）。
pub fn font_scale() -> f32 {
    FONT_SCALE_MILLI.load(Ordering::Relaxed) as f32 / 1000.
}

/// 设置界面字体族；没装的名字忽略（GPUI 在排版找不到的字体族时会 panic，
/// 手改过设置文件也不能让它崩在启动上）。None = 回到内嵌的 Inter。
pub fn set_font_family(cx: &App, family: Option<SharedString>) {
    let checked = family.filter(|name| installed_fonts(cx).iter().any(|f| f == name.as_ref()));
    if let Ok(mut slot) = FONT_FAMILY.write() {
        *slot = checked;
    }
}

/// 系统已装字体族（枚举一次要上百毫秒，全进程缓存）。
pub fn installed_fonts(cx: &App) -> &'static [String] {
    INSTALLED_FONTS.get_or_init(|| cx.text_system().all_font_names())
}

/// 已经缓存好的字体清单；还没枚举过时是空的（枚举要有 `App`，见 [`installed_fonts`]）。
pub fn cached_fonts() -> &'static [String] {
    INSTALLED_FONTS.get().map_or(&[], Vec::as_slice)
}

/// 界面字体族（根视图与代码块右上角这类「非等宽」处用）。
pub fn sans() -> SharedString {
    FONT_FAMILY.read().ok().and_then(|f| f.clone()).unwrap_or_else(|| DEFAULT_SANS.into())
}

/// 当前根字号（1rem 的像素值）：gpui 的间距 / 圆角 / 图标 / 字号全按它换算。
pub fn root_font_size() -> Pixels {
    px(BASE_FONT_SIZE * font_scale())
}

/// 把字号缩放、字体族和色板装进 gpui-kit 的主题（经 `Theme::update` 调用）。
///
/// `gpui-component` 的 Root 每帧把 `Theme::font_size` 写进 `window.set_rem_size()`，
/// 所以这里改完 `font_size`，下一次渲染整套界面（含间距、圆角、图标）就等比缩放。
pub fn apply_to_theme(theme: &mut gpui_kit::component::theme::Theme) {
    theme.font_size = root_font_size();
    // 字体族每次都写：换回内嵌的 Inter 时，组件主题里还留着上一套，
    // 不覆盖的话输入框 / 开关会和根视图用两种字体
    theme.font_family = sans();
    // 组件圆角是 Pixels 不是 rem，按当前根字号换算，才跟着界面缩放：
    // 一般控件（按钮、输入框）对应 `rounded_lg`，对话框 / 通知对应 `rounded_xl`
    theme.radius = rems(0.5).to_pixels(root_font_size());
    theme.radius_lg = rems(0.75).to_pixels(root_font_size());
    // `Theme::change` 切外观时会重新载入 gpui-kit 自带主题，配色要在它之后每次重写
    let dark = theme.mode.is_dark();
    apply_colors(&mut theme.colors, dark);
}

/// 把色板写进 gpui-kit 组件主题的语义色，让 `Button` / `TabBar` / `Kbd` / 弹层 / 滚动条
/// 这些组件直接用就是纸底 + 赤陶橙，不用再逐个覆盖颜色。
///
/// 字段语义沿用 shadcn（见 `gpui-component` 的 `theme/default-theme.json`）。
/// `button_*` 这类派生色在 gpui-kit 里只在载入主题文件时从 `primary` 等推出来，
/// 这里改完基色不会自动跟着变，所以要一并写。
/// 色板里没有对应物的（图表、info / warning / success、基础色相、拖放）保留 gpui-kit 默认。
fn apply_colors(t: &mut gpui_kit::component::theme::ThemeColor, dark: bool) {
    let p = |color: Color| -> gpui_kit::Hsla {
        gpui_kit::rgb(if dark { color.dark } else { color.light }).into()
    };

    // 底、文字、边框
    t.background = p(PAPER);
    t.foreground = p(INK);
    t.border = p(LINE);
    t.input = p(LINE);
    t.ring = p(INK_FAINT);
    t.caret = p(INK);
    t.selection = p(SELECTION);
    t.muted = p(SAND);
    t.muted_foreground = p(INK_SOFT);
    t.accent = p(HOVER);
    t.accent_foreground = p(INK);
    t.popover = p(CARD);
    t.popover_foreground = p(INK);
    t.link = p(INK);
    t.link_hover = p(INK_SOFT);
    t.link_active = p(INK);
    t.window_border = p(LINE);

    // 主按钮：带文字的用 CORAL_DEEP（CORAL 上白字只有 3.1:1）
    t.primary = p(CORAL_DEEP);
    t.primary_hover = p(CORAL_DEEP_HOVER);
    t.primary_active = p(CORAL_DEEP_HOVER);
    t.primary_foreground = p(WHITE);
    t.button_primary = t.primary;
    t.button_primary_hover = t.primary_hover;
    t.button_primary_active = t.primary_active;
    t.button_primary_foreground = t.primary_foreground;

    // 次级按钮：沙色实底，悬停 SELECTED
    t.secondary = p(SAND);
    t.secondary_hover = p(SELECTED);
    t.secondary_active = p(SELECTED);
    t.secondary_foreground = p(INK);
    t.button_secondary = t.secondary;
    t.button_secondary_hover = t.secondary_hover;
    t.button_secondary_active = t.secondary_active;
    t.button_secondary_foreground = t.secondary_foreground;

    // 默认按钮：卡片底 + 描边（描边取 `input`）
    t.button = p(CARD);
    t.button_hover = p(HOVER);
    t.button_active = p(SELECTED);
    t.button_foreground = p(INK);

    // 危险：实色按钮底用 DANGER（RED 是前景色，深色下提亮，不当底）
    t.danger = p(DANGER);
    t.danger_hover = p(DANGER);
    t.danger_active = p(DANGER);
    t.danger_foreground = p(WHITE);
    t.button_danger = t.danger;
    t.button_danger_hover = t.danger_hover;
    t.button_danger_active = t.danger_active;
    t.button_danger_foreground = t.danger_foreground;

    // 列表 / 菜单条目（Select、List、Command）：放在弹层的卡片底上
    t.list = p(CARD);
    t.list_hover = p(HOVER);
    t.list_active = p(SELECTED);
    t.list_active_border = p(LINE_STRONG);
    t.list_even = p(PAPER);
    t.list_head = p(SAND);

    // 表格
    t.table = p(CARD);
    t.table_hover = p(HOVER);
    t.table_active = p(SELECTED);
    t.table_active_border = p(LINE_STRONG);
    t.table_even = p(PAPER);
    t.table_head = p(SAND);
    t.table_head_foreground = p(INK_SOFT);
    t.table_row_border = p(LINE);

    // 标签页：沙色条上浮起卡片色的当前项
    t.tab_bar = p(SAND);
    t.tab_bar_segmented = p(SAND);
    t.tab_active = p(CARD);
    t.tab_foreground = p(INK_SOFT);
    t.tab_active_foreground = p(INK);

    // 开关 / 滑杆 / 进度 / 骨架屏
    t.switch = p(LINE_STRONG);
    t.switch_thumb = p(WHITE);
    t.slider_bar = p(CORAL);
    t.slider_thumb = p(WHITE);
    t.progress_bar = p(CORAL);
    t.skeleton = p(SAND);

    // 滚动条滑块：装饰性，用 INK_FAINT，悬停加深
    t.scrollbar_thumb = p(INK_FAINT);
    t.scrollbar_thumb_hover = p(INK_SOFT);

    // 侧栏 / 分组 / 折叠面板 / 描述列表
    t.sidebar = p(SAND);
    t.sidebar_foreground = p(INK);
    t.sidebar_border = p(LINE);
    t.sidebar_accent = p(SELECTED);
    t.sidebar_accent_foreground = p(INK);
    t.sidebar_primary = p(CORAL_DEEP);
    t.sidebar_primary_foreground = p(WHITE);
    t.group_box = p(SAND);
    t.group_box_foreground = p(INK);
    t.accordion = p(CARD);
    t.description_list_label = p(SAND);
    t.description_list_label_foreground = p(INK_SOFT);
}

/// 直接写字体族状态（不过「是否装了」的校验）；单测用。
#[cfg(test)]
fn set_font_family_raw(family: Option<SharedString>) {
    if let Ok(mut slot) = FONT_FAMILY.write() {
        *slot = family;
    }
}

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
/// 设置页里填数字的小输入框（步数上限这类）
pub const NUMBER_INPUT_W: Pixels = px(120.);

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

    /// 缩放：标准档是 16px，改倍率后根字号等比变，且越界会被夹住。
    #[core::prelude::v1::test]
    fn font_scale_drives_root_size_and_clamps() {
        set_font_scale(1.0);
        assert_eq!(root_font_size(), px(16.));
        // 字号阶梯本身是 rem：跟着根字号走
        assert_eq!(TEXT_LARGE.to_pixels(root_font_size()), px(15.));
        assert_eq!(TEXT_TITLE.to_pixels(root_font_size()), px(28.));

        set_font_scale(1.25);
        assert_eq!(font_scale(), 1.25);
        assert_eq!(root_font_size(), px(20.));
        assert_eq!(TEXT_LARGE.to_pixels(root_font_size()), px(18.75));

        // 越界夹回上下限
        set_font_scale(9.0);
        assert_eq!(font_scale(), FONT_SCALE_MAX);
        set_font_scale(0.1);
        assert_eq!(font_scale(), FONT_SCALE_MIN);
        set_font_scale(1.0);
    }

    /// 组件主题的配色按外观取色板，派生的按钮色跟着基色走。
    #[core::prelude::v1::test]
    fn component_colors_follow_palette() {
        let hsla = |v: u32| -> gpui_kit::Hsla { gpui_kit::rgb(v).into() };
        let mut t = gpui_kit::component::theme::ThemeColor::default();

        apply_colors(&mut t, false);
        assert_eq!(t.background, hsla(PAPER.light));
        assert_eq!(t.primary, hsla(CORAL_DEEP.light));
        assert_eq!(t.button_primary, t.primary);
        assert_eq!(t.button_primary_foreground, hsla(WHITE.light));
        assert_eq!(t.button_secondary_hover, hsla(SELECTED.light));

        apply_colors(&mut t, true);
        assert_eq!(t.background, hsla(PAPER.dark));
        assert_eq!(t.foreground, hsla(INK.dark));
        assert_eq!(t.danger, hsla(DANGER.dark));
        assert_eq!(t.button_danger, t.danger);
    }

    /// 字体族：没选过时回到内嵌 Inter；`cached_fonts` 在没枚举过时返回空表而不是 panic。
    #[core::prelude::v1::test]
    fn font_family_defaults_to_embedded_inter() {
        // 单测里没有 `App`，系统字体多半还没枚举；关键是取它不会炸
        let _ = cached_fonts();
        assert_eq!(sans(), DEFAULT_SANS);
        // 直接写内部状态（`set_font_family` 要 `App`，单测里拿不到）
        set_font_family_raw(Some("Microsoft YaHei".into()));
        assert_eq!(sans(), "Microsoft YaHei");
        set_font_family_raw(None);
        assert_eq!(sans(), DEFAULT_SANS);
    }
}
