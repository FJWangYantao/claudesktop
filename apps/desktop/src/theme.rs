//! 色板、字体与字号阶梯（Claude 品牌基准：暖纸底 + 赤陶橙）。

use gpui_kit::{
    App, BoxShadow, DefiniteLength, FontFallbacks, FontFeatures, Hsla, Pixels, Rems, Rgba,
    SharedString, hsla, px, relative, rems,
};
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

// ---------- 色板 ----------
// `Color` 是语义 token（不是色值）：界面里照旧写 `rgb(INK)`，取值查当前色板 + 当前明暗
// （这里的 `rgb` 遮住了 gpui 的同名函数，各文件要 `use crate::theme::rgb;`）。
// 新颜色：在 `Color` 和 `Palette` 各加一项，`WARM` 给值；别的色板不写就继承 `WARM`
// （状态色、diff 就是这么继承的），要写就浅色 / 深色都给，并让 `palette_contrast` 测试通过。

/// 一个语义颜色的浅色 / 深色取值（`0xRRGGBB`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shade {
    pub light: u32,
    pub dark: u32,
}

const fn s(light: u32, dark: u32) -> Shade {
    Shade { light, dark }
}

/// 语义颜色 token。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    Paper,
    Sand,
    SandActive,
    Card,
    Coral,
    CoralDeep,
    CoralDeepHover,
    OnAccent,
    Ink,
    InkSoft,
    InkFaint,
    Line,
    LineStrong,
    CodeBg,
    Hover,
    HoverStrong,
    Selection,
    Heat1,
    Heat2,
    Heat3,
    Heat4,
    Green,
    Red,
    Danger,
    White,
    DiffAddBg,
    DiffAddFg,
    DiffDelBg,
    DiffDelFg,
}

/// 一套配色：每个 token 的浅色 / 深色取值。
#[derive(Clone, Copy)]
pub struct Palette {
    pub paper: Shade,
    pub sand: Shade,
    pub sand_active: Shade,
    pub card: Shade,
    pub coral: Shade,
    pub coral_deep: Shade,
    pub coral_deep_hover: Shade,
    pub on_accent: Shade,
    pub ink: Shade,
    pub ink_soft: Shade,
    pub ink_faint: Shade,
    pub line: Shade,
    pub line_strong: Shade,
    pub code_bg: Shade,
    pub hover: Shade,
    pub hover_strong: Shade,
    pub selection: Shade,
    pub heat1: Shade,
    pub heat2: Shade,
    pub heat3: Shade,
    pub heat4: Shade,
    pub green: Shade,
    pub red: Shade,
    pub danger: Shade,
    pub white: Shade,
    pub diff_add_bg: Shade,
    pub diff_add_fg: Shade,
    pub diff_del_bg: Shade,
    pub diff_del_fg: Shade,
}

impl Palette {
    /// 穷尽 match：`Color` 加了项而这里没接上会编译不过。
    pub const fn shade(&self, color: Color) -> Shade {
        match color {
            Color::Paper => self.paper,
            Color::Sand => self.sand,
            Color::SandActive => self.sand_active,
            Color::Card => self.card,
            Color::Coral => self.coral,
            Color::CoralDeep => self.coral_deep,
            Color::CoralDeepHover => self.coral_deep_hover,
            Color::OnAccent => self.on_accent,
            Color::Ink => self.ink,
            Color::InkSoft => self.ink_soft,
            Color::InkFaint => self.ink_faint,
            Color::Line => self.line,
            Color::LineStrong => self.line_strong,
            Color::CodeBg => self.code_bg,
            Color::Hover => self.hover,
            Color::HoverStrong => self.hover_strong,
            Color::Selection => self.selection,
            Color::Heat1 => self.heat1,
            Color::Heat2 => self.heat2,
            Color::Heat3 => self.heat3,
            Color::Heat4 => self.heat4,
            Color::Green => self.green,
            Color::Red => self.red,
            Color::Danger => self.danger,
            Color::White => self.white,
            Color::DiffAddBg => self.diff_add_bg,
            Color::DiffAddFg => self.diff_add_fg,
            Color::DiffDelBg => self.diff_del_bg,
            Color::DiffDelFg => self.diff_del_fg,
        }
    }
}

/// 暖纸（Claude 品牌：暖纸底 + 赤陶橙）
const WARM: Palette = Palette {
    paper: s(0xFAF9F5, 0x262624),
    sand: s(0xF0EEE6, 0x1F1E1D),
    sand_active: s(0xE8E3D6, 0x33322F),
    card: s(0xFFFFFF, 0x30302E),
    coral: s(0xD97757, 0xD97757),
    coral_deep: s(0xBF502B, 0xBF502B),
    coral_deep_hover: s(0xAF4928, 0xAF4928),
    on_accent: s(0xFFFFFF, 0xFFFFFF),
    ink: s(0x1F1E1D, 0xF5F4EE),
    ink_soft: s(0x6E6B65, 0xBDB9AE),
    ink_faint: s(0xB5B0A4, 0x6E6C66),
    line: s(0xE5E1D5, 0x3D3C38),
    line_strong: s(0xD3CEC1, 0x55534E),
    code_bg: s(0xEDE9DF, 0x1A1918),
    hover: s(0xF0EEE6, 0x3A3936),
    hover_strong: s(0xDCD6C6, 0x45443F),
    selection: s(0xF3D9CD, 0x5C3B2E),
    heat1: s(0xF3D9CD, 0x4A3329),
    heat2: s(0xEBB39C, 0x7A4632),
    heat3: s(0xD97757, 0xB85F42),
    heat4: s(0xAE5236, 0xE3866A),
    green: s(0x7A9A65, 0x94B07E),
    red: s(0xC0504D, 0xE5726C),
    danger: s(0xC0504D, 0xB8433F),
    white: s(0xFFFFFF, 0xFFFFFF),
    diff_add_bg: s(0xE4F3DE, 0x243222),
    diff_add_fg: s(0x3C6B2E, 0x9FCC8C),
    diff_del_bg: s(0xFBE3DF, 0x3A2422),
    diff_del_fg: s(0xA4443A, 0xF19B90),
};

/// 墨纸（Flexoki：底更黄、墨更黑，青绿强调）
const FLEXOKI: Palette = Palette {
    paper: s(0xFFFCF0, 0x1C1B1A),
    sand: s(0xF2F0E5, 0x100F0F),
    sand_active: s(0xE6E4D9, 0x343331),
    card: s(0xFFFFFF, 0x282726),
    coral: s(0x24837B, 0x3AA99F),
    coral_deep: s(0x24837B, 0x3AA99F),
    coral_deep_hover: s(0x1C6C66, 0x5ABDAC),
    on_accent: s(0xFFFFFF, 0x100F0F),
    ink: s(0x100F0F, 0xCECDC3),
    ink_soft: s(0x64635E, 0xA8A69E),
    ink_faint: s(0xB7B5AC, 0x575653),
    line: s(0xE6E4D9, 0x343331),
    line_strong: s(0xCECDC3, 0x4D4B48),
    code_bg: s(0xF2F0E5, 0x100F0F),
    hover: s(0xF2F0E5, 0x343331),
    hover_strong: s(0xDAD8CE, 0x403E3C),
    selection: s(0xBFE8D9, 0x164743),
    heat1: s(0xBFE8D9, 0x143F3C),
    heat2: s(0x87D3C3, 0x1C6C66),
    heat3: s(0x3AA99F, 0x24837B),
    heat4: s(0x1C6C66, 0x5ABDAC),
    ..WARM
};

/// 玫瑰松（Rosé Pine Dawn / Moon：低饱和的玫瑰和紫灰）
const ROSE_PINE: Palette = Palette {
    paper: s(0xFAF4ED, 0x232136),
    sand: s(0xF2E9E1, 0x1F1D2E),
    sand_active: s(0xE9DFD6, 0x393552),
    card: s(0xFFFAF3, 0x2A273F),
    coral: s(0xCF7672, 0xEA9A97),
    coral_deep: s(0x286983, 0xC4A7E7),
    coral_deep_hover: s(0x1F5468, 0xD3BDF0),
    on_accent: s(0xFFFFFF, 0x232136),
    ink: s(0x575279, 0xE0DEF4),
    ink_soft: s(0x625E7E, 0xA8A4C0),
    ink_faint: s(0x9893A5, 0x6E6A86),
    line: s(0xDFDAD9, 0x44415A),
    line_strong: s(0xCECACD, 0x56526E),
    code_bg: s(0xF2E9E1, 0x1F1D2E),
    hover: s(0xF4EDE8, 0x393552),
    hover_strong: s(0xDFDAD9, 0x44415A),
    selection: s(0xF2D9D4, 0x44415A),
    heat1: s(0xF2D9D4, 0x4A3A4F),
    heat2: s(0xE8B3AE, 0x7A5566),
    heat3: s(0xD7827E, 0xB97A80),
    heat4: s(0xB4637A, 0xEA9A97),
    ..WARM
};

/// 常青（Everforest：米黄底配灰绿，护眼）
const EVERFOREST: Palette = Palette {
    paper: s(0xFDF6E3, 0x2D353B),
    sand: s(0xEFEBD4, 0x232A2E),
    sand_active: s(0xE6E2CC, 0x3D484D),
    card: s(0xFFFBEF, 0x343F44),
    coral: s(0x839700, 0xA7C080),
    coral_deep: s(0x5F7A00, 0xA7C080),
    coral_deep_hover: s(0x506800, 0xB8CE94),
    on_accent: s(0xFFFFFF, 0x2D353B),
    ink: s(0x4F5B62, 0xD3C6AA),
    ink_soft: s(0x56645B, 0xB0BBB2),
    ink_faint: s(0xA6B0A0, 0x7A8478),
    line: s(0xE0DCC7, 0x475258),
    line_strong: s(0xBDC3AF, 0x56635F),
    code_bg: s(0xF4F0D9, 0x232A2E),
    hover: s(0xF4F0D9, 0x3D484D),
    hover_strong: s(0xE0DCC7, 0x475258),
    selection: s(0xEAEDC8, 0x425047),
    heat1: s(0xDCE3AE, 0x425047),
    heat2: s(0xB9C766, 0x5E7356),
    heat3: s(0x8DA101, 0x83996A),
    heat4: s(0x5F7A00, 0xA7C080),
    ..WARM
};

/// 北境（Nord：雪白 + 冰蓝，唯一冷色系）
const NORD: Palette = Palette {
    paper: s(0xECEFF4, 0x2E3440),
    sand: s(0xE5E9F0, 0x272C36),
    sand_active: s(0xD8DEE9, 0x3B4252),
    card: s(0xF8F9FB, 0x3B4252),
    coral: s(0x5E81AC, 0x88C0D0),
    coral_deep: s(0x4C6A92, 0x88C0D0),
    coral_deep_hover: s(0x3F5A7D, 0x9DCBD8),
    on_accent: s(0xFFFFFF, 0x2E3440),
    ink: s(0x2E3440, 0xECEFF4),
    ink_soft: s(0x4C566A, 0xB4BCCB),
    ink_faint: s(0x9AA3B5, 0x616E88),
    line: s(0xD8DEE9, 0x434C5E),
    line_strong: s(0xC2CAD8, 0x4C566A),
    code_bg: s(0xE5E9F0, 0x272C36),
    hover: s(0xE5E9F0, 0x434C5E),
    hover_strong: s(0xCBD3E0, 0x4C566A),
    selection: s(0xCFDCEC, 0x434C5E),
    heat1: s(0xC9D8E8, 0x3E5566),
    heat2: s(0x9DB8D4, 0x4F7A8C),
    heat3: s(0x5E81AC, 0x6FA3B4),
    heat4: s(0x3F5A7D, 0x88C0D0),
    ..WARM
};

/// 拿铁 / 摩卡（Catppuccin Latte / Mocha：粉彩紫，最活泼）
const CATPPUCCIN: Palette = Palette {
    paper: s(0xEFF1F5, 0x1E1E2E),
    sand: s(0xE6E9EF, 0x181825),
    sand_active: s(0xDCE0E8, 0x313244),
    card: s(0xFFFFFF, 0x313244),
    coral: s(0x8839EF, 0xCBA6F7),
    coral_deep: s(0x8839EF, 0xCBA6F7),
    coral_deep_hover: s(0x7529D6, 0xD8BCFA),
    on_accent: s(0xFFFFFF, 0x1E1E2E),
    ink: s(0x4C4F69, 0xCDD6F4),
    ink_soft: s(0x5C5F77, 0xBAC2DE),
    ink_faint: s(0x9CA0B0, 0x6C7086),
    line: s(0xCCD0DA, 0x45475A),
    line_strong: s(0xBCC0CC, 0x585B70),
    code_bg: s(0xE6E9EF, 0x181825),
    hover: s(0xE6E9EF, 0x45475A),
    hover_strong: s(0xCCD0DA, 0x585B70),
    selection: s(0xDCCBF7, 0x4B3F66),
    heat1: s(0xDCCBF7, 0x45385E),
    heat2: s(0xB98CF5, 0x6E5394),
    heat3: s(0x8839EF, 0x9C7BC8),
    heat4: s(0x6A1FCF, 0xCBA6F7),
    ..WARM
};

/// GitHub（Primer 浅色 / 深色：白底、浅灰凹面、蓝链接）
///
/// 唯一一套「强调色是蓝」的配色（见「设计指南-Claude风格UI.md」：暖纸刻意避开蓝）。
/// 浅色纸底和卡片底都是纯白，凹面靠 `sand`（`#f6f8fa`）拉开；强调色取 GitHub 的链接蓝，
/// 深色版悬停压到 `#1760cf`——原版 `#388bfd` 上白字只有 3.3:1。
const GITHUB: Palette = Palette {
    paper: s(0xFFFFFF, 0x0D1117),
    sand: s(0xF6F8FA, 0x010409),
    sand_active: s(0xEAEEF2, 0x161B22),
    card: s(0xFFFFFF, 0x161B22),
    coral: s(0x0969DA, 0x1F6FEB),
    coral_deep: s(0x0969DA, 0x1F6FEB),
    coral_deep_hover: s(0x0550AE, 0x1760CF),
    on_accent: s(0xFFFFFF, 0xFFFFFF),
    ink: s(0x1F2328, 0xE6EDF3),
    ink_soft: s(0x59636E, 0x9198A1),
    ink_faint: s(0x818B98, 0x6E7681),
    line: s(0xD1D9E0, 0x3D444D),
    line_strong: s(0xB7BEC8, 0x515B65),
    code_bg: s(0xF6F8FA, 0x161B22),
    hover: s(0xF6F8FA, 0x1C2128),
    hover_strong: s(0xE7ECF0, 0x2D333B),
    selection: s(0xDDF4FF, 0x0C2D6B),
    heat1: s(0x9BE9A8, 0x0E4429),
    heat2: s(0x40C463, 0x006D32),
    heat3: s(0x30A14E, 0x26A641),
    heat4: s(0x216E39, 0x39D353),
    green: s(0x1A7F37, 0x3FB950),
    red: s(0xCF222E, 0xFF7B72),
    danger: s(0xCF222E, 0xDA3633),
    white: s(0xFFFFFF, 0xFFFFFF),
    diff_add_bg: s(0xE6FFEC, 0x12261E),
    diff_add_fg: s(0x1A7F37, 0x3FB950),
    diff_del_bg: s(0xFFEBE9, 0x25171C),
    diff_del_fg: s(0xCF222E, 0xFF7B72),
};

/// 纯白（只用灰阶：纸底和卡片是纯白，强调色就是墨色本身）
///
/// 没有彩色强调：主按钮是近黑实底、浅灰悬停，选中 / 聚焦全用灰阶。
/// 深色版是纯黑纸底 + 近白按钮底（`ON_ACCENT` 反过来用黑字）。
const PURE: Palette = Palette {
    paper: s(0xFFFFFF, 0x000000),
    sand: s(0xF2F2F2, 0x0A0A0A),
    sand_active: s(0xE6E6E6, 0x141414),
    card: s(0xFFFFFF, 0x0A0A0A),
    coral: s(0x1F2328, 0xE6EDF3),
    coral_deep: s(0x1F2328, 0xF0F6FC),
    coral_deep_hover: s(0x32383F, 0xFFFFFF),
    on_accent: s(0xFFFFFF, 0x000000),
    ink: s(0x1F2328, 0xEDEDED),
    ink_soft: s(0x575757, 0xA0A0A0),
    ink_faint: s(0x8A8A8A, 0x6E6E6E),
    line: s(0xE5E5E5, 0x262626),
    line_strong: s(0xC8C8C8, 0x3D3D3D),
    code_bg: s(0xF6F6F6, 0x0A0A0A),
    hover: s(0xF2F2F2, 0x141414),
    hover_strong: s(0xE2E2E2, 0x212121),
    selection: s(0xE2E2E2, 0x1F1F1F),
    heat1: s(0xEDEDED, 0x1F1F1F),
    heat2: s(0xD4D4D4, 0x3A3A3A),
    heat3: s(0xA8A8A8, 0x5C5C5C),
    heat4: s(0x575757, 0xA8A8A8),
    ..WARM
};

/// 顺序和 `settings::ThemeChoice::ALL` 一致（`ThemeChoice::index` 取下标）。
pub static PALETTES: [Palette; 8] =
    [WARM, FLEXOKI, ROSE_PINE, EVERFOREST, NORD, CATPPUCCIN, GITHUB, PURE];

static PALETTE: AtomicUsize = AtomicUsize::new(0);

/// 换配色（之后要重写组件主题并重绘，见 `ChatApp::apply_font`）。
pub fn set_palette(index: usize) {
    PALETTE.store(index.min(PALETTES.len() - 1), Ordering::Relaxed);
}

pub fn palette_index() -> usize {
    PALETTE.load(Ordering::Relaxed)
}

fn palette() -> &'static Palette {
    &PALETTES[palette_index()]
}

static DARK: AtomicBool = AtomicBool::new(false);

/// 切换外观（由根视图按设置 / 系统外观调用，之后要重绘）。
pub fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// 某套色板里某个 token 在给定明暗下的取值。
fn pick(palette: &Palette, color: Color, dark: bool) -> u32 {
    let shade = palette.shade(color);
    if dark { shade.dark } else { shade.light }
}

/// 按当前色板和外观取颜色。
pub fn rgb(color: Color) -> Rgba {
    gpui_kit::rgb(pick(palette(), color, is_dark()))
}

/// 同一个颜色加一层透明度（0..1）：只用在「状态色描边」「光晕」这类不该抢前景的地方。
pub fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: color.a * alpha, ..color }
}

/// 语义色的 `Hsla` 版本（`BoxShadow` 要这个）。
fn hsla_of(color: Color) -> Hsla {
    rgb(color).into()
}

/// 配色菜单的色块：画别的色板（不是当前这套）在当前明暗下的颜色。
pub fn swatch(index: usize, color: Color) -> Rgba {
    gpui_kit::rgb(pick(&PALETTES[index.min(PALETTES.len() - 1)], color, is_dark()))
}

// 常量名保留，调用点不用改
/// 主区底色（纸）
pub const PAPER: Color = Color::Paper;
/// 凹下去的面：侧栏、用户气泡、代码块、概览面板、chip（深色下比主区更暗）
pub const SAND: Color = Color::Sand;
pub const SAND_ACTIVE: Color = Color::SandActive;
/// 浮起来的面：输入框、卡片、弹层
pub const CARD: Color = Color::Card;
pub const CORAL: Color = Color::Coral;
/// 带文字的主按钮底色：与 CORAL 同色相、压暗一档（白字 ≥ 4.5:1）。
/// CORAL 上的白字只有 3:1 上下，只够图标用（如发送按钮）
pub const CORAL_DEEP: Color = Color::CoralDeep;
/// 带文字的主按钮悬停
pub const CORAL_DEEP_HOVER: Color = Color::CoralDeepHover;
/// 主按钮（CORAL_DEEP / CORAL 底）上的文字：有的配色深色版按钮底很亮，白字看不清
pub const ON_ACCENT: Color = Color::OnAccent;
pub const INK: Color = Color::Ink;
/// 次要文字：说明、元信息、时间（各色板各外观都 ≥ 4.5:1）
pub const INK_SOFT: Color = Color::InkSoft;
/// 只给占位、禁用、分隔符和装饰用（纸底上约 2–3:1）——承载信息的文字用 `INK_SOFT`
pub const INK_FAINT: Color = Color::InkFaint;
pub const LINE: Color = Color::Line;
/// 悬停时加深一档的边框（输入框），介于 `LINE` 和聚焦用的 `INK_FAINT` 之间
pub const LINE_STRONG: Color = Color::LineStrong;
pub const GREEN: Color = Color::Green;
/// 红色文字 / 小圆点（深色下提亮，只适合放在底色上当前景）
pub const RED: Color = Color::Red;
/// 危险操作的实色按钮底：删除确认、窗口关闭悬停
pub const DANGER: Color = Color::Danger;
/// 实色按钮上的图标 / 文字（两种外观都是白）
pub const WHITE: Color = Color::White;
/// 透明（配 gpui 的 `rgba` 用，与外观无关）
pub const CLEAR: u32 = 0x00000000;
/// 行内代码底色
pub const CODE_BG: Color = Color::CodeBg;
/// 用量热力图：0 = 空白格，1..=4 由浅到深（深色下由暗到亮）的强调色
pub const HEAT: [Color; 5] = [SAND_ACTIVE, Color::Heat1, Color::Heat2, Color::Heat3, Color::Heat4];

// ---------- 交互态 ----------
// 同一种底上的悬停永远是同一个颜色；不要再用 LINE / PAPER / 透明度充当悬停。

/// 纸底 / 卡片上的悬停
pub const HOVER: Color = Color::Hover;
/// 选中态，以及沙色底（侧栏、概览面板）上的悬停
pub const SELECTED: Color = SAND_ACTIVE;
/// 已经是 `SELECTED` 的行里再叠一层的悬停（侧栏会话行上的小图标按钮）
pub const HOVER_STRONG: Color = Color::HoverStrong;
// 字体：官方的 Styrene 无法分发，用 OFL 近似款，内嵌见 `fonts.rs`。
// 官方 Code 桌面端正文是无衬线，不再用衬线（中文回退成雅黑后会和衬线拉丁字母混排）。
/// 默认界面字体族（`fonts.rs` 内嵌的 Inter）
pub const DEFAULT_SANS: &str = "Inter";
/// 代码 / 工具行的等宽字体（`fonts.rs` 内嵌；不随设置变化，代码靠它对齐）
pub const MONO: &str = "JetBrains Mono";
pub const DIFF_ADD_BG: Color = Color::DiffAddBg;
pub const DIFF_ADD_FG: Color = Color::DiffAddFg;
pub const DIFF_DEL_BG: Color = Color::DiffDelBg;
pub const DIFF_DEL_FG: Color = Color::DiffDelFg;
/// 文字选区底色（输入框、Markdown）：淡强调色
pub const SELECTION: Color = Color::Selection;

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
/// 次级标题：统计数值、回复里的三级标题（17px）
pub const TEXT_LEAD: Rems = rems(1.0625);
/// 分区标题、回复里的二级标题（20px）
pub const TEXT_HEADING: Rems = rems(1.25);
/// 回复里的一级标题（24px）
pub const TEXT_DISPLAY: Rems = rems(1.5);
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
    // Markdown 的行内代码 / 代码块取这一项（不设是平台默认：Windows 上 Consolas），和工具行统一用内嵌的等宽
    theme.mono_font_family = MONO.into();
    // 组件圆角是 Pixels 不是 rem，按当前根字号换算，才跟着界面缩放：
    // 一般控件（按钮、输入框）对应 `rounded_lg`，对话框 / 通知对应 `rounded_xl`
    theme.radius = rems(0.5).to_pixels(root_font_size());
    theme.radius_lg = rems(0.75).to_pixels(root_font_size());
    // `Theme::change` 切外观时会重新载入 gpui-kit 自带主题，配色要在它之后每次重写
    let dark = theme.mode.is_dark();
    apply_colors(&mut theme.colors, palette(), dark);
}

/// 把色板写进 gpui-kit 组件主题的语义色，让 `Button` / `TabBar` / `Kbd` / 弹层 / 滚动条
/// 这些组件直接用就是纸底 + 强调色，不用再逐个覆盖颜色。
///
/// 字段语义沿用 shadcn（见 `gpui-component` 的 `theme/default-theme.json`）。
/// `button_*` 这类派生色在 gpui-kit 里只在载入主题文件时从 `primary` 等推出来，
/// 这里改完基色不会自动跟着变，所以要一并写。
/// 色板里没有对应物的（图表、info / warning / success、基础色相、拖放）保留 gpui-kit 默认。
fn apply_colors(t: &mut gpui_kit::component::theme::ThemeColor, pal: &Palette, dark: bool) {
    let p = |color: Color| -> gpui_kit::Hsla { gpui_kit::rgb(pick(pal, color, dark)).into() };

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

    // 主按钮：带文字的用 CORAL_DEEP（CORAL 上白字只有 3:1 上下）；文字用 ON_ACCENT
    // （有的配色深色版按钮底很亮，白字看不清）
    t.primary = p(CORAL_DEEP);
    t.primary_hover = p(CORAL_DEEP_HOVER);
    t.primary_active = p(CORAL_DEEP_HOVER);
    t.primary_foreground = p(ON_ACCENT);
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
    t.sidebar_primary_foreground = p(ON_ACCENT);
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

// ---------- 阴影 ----------
// gpui 的 `BoxShadow::blur_radius` 是高斯 σ，CSS 的 blur 是 2σ，所以照网页配方写时要**减半**。
// `.shadow_xs/sm/md/lg` 是宏生成的 Tailwind 原值（没减半），比网页宽一倍，别再拿来当设计值用；
// 这里的两个函数按「减半后」的 σ 给。
//
// 两层的思路（原版 `--cds-shadow-lg`）：近距一层小锐利做接触感，远距一层大扩散做悬浮感。
// 只有一层会像贴纸。墨水用固定的 8% 黑：阴影不是语义色，深色下也一样读得出来。

/// 悬浮面（输入框、卡片悬停、回到底部这类浮在纸上的东西）的两层阴影。
/// 对应原版 `0 4px 8px` + `0 12px 28px -2px`，σ 减半。
pub fn float_shadow() -> Vec<BoxShadow> {
    let ink = hsla(0., 0., 0., 0.08);
    vec![
        BoxShadow::new(px(0.), px(2.), ink).blur_radius(px(4.)),
        BoxShadow::new(px(0.), px(6.), ink).blur_radius(px(14.)).spread_radius(px(-1.)),
    ]
}

/// 贴着容器浮起一档的东西（输入框静止态）：比 [`float_shadow`] 收一档，不抢注意力。
pub fn raised_shadow() -> Vec<BoxShadow> {
    let ink = hsla(0., 0., 0., 0.05);
    vec![BoxShadow::new(px(0.), px(1.), ink).blur_radius(px(3.))]
}

/// 三层焦点环（原版 `--cds-focus-shadow`），都画在元素边框**外面**：
/// ① 贴着边框一圈 1px 外部底色，让环和元素之间留一道缝；
/// ② 再往外 1px 实边（spread 2px，里面那 1px 被 ① 盖住）；
/// ③ 6px 光晕（σ 3px，CSS 值减半），40% 透明度。
///
/// 不用 `inset`：内阴影画在边框底下（`Style::paint` 先画内阴影再画边框），有边框的元素上看不见。
/// gpui 里后插入的阴影盖在前面的上面（和 CSS「第一层在最上」相反），所以 vec 里是 ③ ② ① 的顺序。
///
/// 描边宽度不跟字号走，所以这里是少数**该用 `px`** 的地方。
/// `accent` 传 `INK_FAINT`（输入框这种中性的）/ `CORAL`（按钮、链接）/ `RED`（危险操作）。
/// ① 的缝取元素**外面**的底色，默认 `PAPER`（输入框放在纸底上）；放在卡片里的元素用
/// [`focus_ring_on`] 传 `CARD`，否则会看到一道和周围不一样的缝。
pub fn focus_ring(accent: Color) -> Vec<BoxShadow> {
    focus_ring_on(accent, PAPER)
}

/// 同 [`focus_ring`]，缝的颜色（元素外面的底色）自己给（见那里的说明）。
pub fn focus_ring_on(accent: Color, gap: Color) -> Vec<BoxShadow> {
    let line = hsla_of(accent);
    vec![
        BoxShadow::new(px(0.), px(0.), line.alpha(line.a * 0.4))
            .blur_radius(px(3.))
            .spread_radius(px(2.)),
        BoxShadow::new(px(0.), px(0.), line).spread_radius(px(2.)),
        BoxShadow::new(px(0.), px(0.), hsla_of(gap)).spread_radius(px(1.)),
    ]
}

/// 状态色（红 / 绿 / 待处理的赤陶）的描边：前景色 40%，直接拿前景色描边会喧宾夺主（原版状态描边一律取浅档）。
pub fn state_line(color: Color) -> Rgba {
    with_alpha(rgb(color), 0.4)
}

// ---------- 布局宽度 ----------
// 阅读列 / 设置页内容 / 概览面板是「measure」（按一行放多少字定），**保持 px**：
// 字号放大时列宽不该跟着放大，否则一行字数跟着变。
// 侧栏、分类导航、顶栏高度是界面的「框」，跟字号缩放（放大后标题和相对时间才不会互挤）。

/// 对话列与输入框的最大宽度（阅读列）
pub const READING_W: Pixels = px(760.);
/// 设置页内容最大宽度
pub const FORM_W: Pixels = px(720.);
/// 设置页左侧分类导航宽度
pub const SETTINGS_NAV_W: Rems = rems(12.5);
/// 首页用量概览面板宽度
pub const OVERVIEW_W: Pixels = px(724.);
/// 侧栏宽度（含外圈留白）
pub const SIDEBAR_W: Rems = rems(16.);
/// 设置页里填数字的小输入框（步数上限这类）
pub const NUMBER_INPUT_W: Pixels = px(120.);

/// 设置页（含用量页）整页的最大宽度：左侧分类导航 + 右侧内容。
/// 导航按 rem 缩放，内容按 measure 固定，所以这里要换算一次；拿不到 `window` 的地方用它。
pub fn settings_page_w() -> Pixels {
    SETTINGS_NAV_W.to_pixels(root_font_size()) + FORM_W
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// 色板、明暗、字号、字体族是进程级全局状态，cargo test 默认多线程并行：
    /// 改它们（或读了要断言）的测试先拿这把锁，否则会读到别的测试刚设的值。
    fn theme_globals() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        // 别的测试断言失败会毒化锁；状态每个测试开头自己设，接着用就行
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[core::prelude::v1::test]
    fn rgb_follows_palette_and_appearance() {
        let _g = theme_globals();
        set_palette(0);
        set_dark(false);
        assert_eq!(rgb(PAPER), gpui_kit::rgb(0xFAF9F5));
        set_dark(true);
        assert_eq!(rgb(PAPER), gpui_kit::rgb(0x262624));
        set_palette(1);
        assert_eq!(rgb(PAPER), gpui_kit::rgb(0x1C1B1A));
        assert_eq!(rgb(GREEN), gpui_kit::rgb(0x94B07E)); // 状态色继承暖纸
        set_palette(0);
        set_dark(false);
        // 赤陶橙两边同色
        assert_eq!(rgb(CORAL), gpui_kit::rgb(0xD97757));
    }

    /// 色板顺序要和设置里的选项一一对应（`ThemeChoice::index` 取下标）。
    #[core::prelude::v1::test]
    fn palettes_line_up_with_settings() {
        assert_eq!(PALETTES.len(), crate::settings::ThemeChoice::ALL.len());
    }

    /// 每套色板、两种明暗下，承载信息的文字都要 ≥ 4.5:1，实底上的图标（发送箭头）≥ 3:1。
    #[core::prelude::v1::test]
    fn palette_contrast() {
        fn lum(v: u32) -> f64 {
            let ch = |s: u32| {
                let x = ((v >> s) & 0xFF) as f64 / 255.;
                if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
        }
        let ratio = |a, b| {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        };
        let pairs = [
            (INK, PAPER, 4.5),
            (INK_SOFT, PAPER, 4.5),
            (INK_SOFT, SAND, 4.5),
            (INK_SOFT, CARD, 4.5),
            (INK_SOFT, HOVER, 4.5),
            (INK, SELECTED, 4.5),
            (INK, CODE_BG, 4.5),
            (INK, SELECTION, 4.5),
            (ON_ACCENT, CORAL_DEEP, 4.5),
            (ON_ACCENT, CORAL_DEEP_HOVER, 4.5),
            (ON_ACCENT, CORAL, 3.0),
        ];
        for (i, pal) in PALETTES.iter().enumerate() {
            for dark in [false, true] {
                for (fg, bg, min) in pairs {
                    let r = ratio(pick(pal, fg, dark), pick(pal, bg, dark));
                    assert!(
                        r >= min,
                        "色板 {i}（{}）{fg:?} / {bg:?} 只有 {r:.2}:1",
                        if dark { "深色" } else { "浅色" }
                    );
                }
            }
        }
    }

    /// 缩放：标准档是 16px，改倍率后根字号等比变，且越界会被夹住。
    #[core::prelude::v1::test]
    fn font_scale_drives_root_size_and_clamps() {
        let _g = theme_globals();
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
        let pal = &PALETTES[0];
        let mut t = gpui_kit::component::theme::ThemeColor::default();

        apply_colors(&mut t, pal, false);
        assert_eq!(t.background, hsla(pick(pal, PAPER, false)));
        assert_eq!(t.primary, hsla(pick(pal, CORAL_DEEP, false)));
        assert_eq!(t.button_primary, t.primary);
        assert_eq!(t.button_primary_foreground, hsla(pick(pal, ON_ACCENT, false)));
        assert_eq!(t.button_secondary_hover, hsla(pick(pal, SELECTED, false)));

        apply_colors(&mut t, pal, true);
        assert_eq!(t.background, hsla(pick(pal, PAPER, true)));
        assert_eq!(t.foreground, hsla(pick(pal, INK, true)));
        assert_eq!(t.danger, hsla(pick(pal, DANGER, true)));
        assert_eq!(t.button_danger, t.danger);
    }

    /// 字体族：没选过时回到内嵌 Inter；`cached_fonts` 在没枚举过时返回空表而不是 panic。
    #[core::prelude::v1::test]
    fn font_family_defaults_to_embedded_inter() {
        let _g = theme_globals();
        // 单测里没有 `App`，系统字体多半还没枚举；关键是取它不会炸
        let _ = cached_fonts();
        assert_eq!(sans(), DEFAULT_SANS);
        // 直接写内部状态（`set_font_family` 要 `App`，单测里拿不到）
        set_font_family_raw(Some("Microsoft YaHei".into()));
        assert_eq!(sans(), "Microsoft YaHei");
        set_font_family_raw(None);
        assert_eq!(sans(), DEFAULT_SANS);
    }

    /// 侧栏 / 分类导航 / 顶栏是「框」，必须跟着字号缩放；阅读列这类 measure 保持 px。
    ///
    /// 用一个只收 `Rems` 的函数卡类型：有人把它们改回 `Pixels` 就编译不过，
    /// 不用等到肉眼发现「放大字号后侧栏标题和相对时间互挤」。
    #[core::prelude::v1::test]
    fn layout_boxes_scale_with_font() {
        let _g = theme_globals();
        const fn as_rems(v: Rems) -> Rems {
            v
        }
        let scale = 1.4;
        set_font_scale(scale);
        let rem = root_font_size();
        // 放大后侧栏、导航、顶栏跟着变宽变高
        assert_eq!(as_rems(SIDEBAR_W).to_pixels(rem), px(256. * scale));
        assert_eq!(as_rems(SETTINGS_NAV_W).to_pixels(rem), px(200. * scale));
        // 阅读列是 measure：不跟字号走（一行字数的量纲不变）
        assert_eq!(READING_W, px(760.));
        assert_eq!(FORM_W, px(720.));
        set_font_scale(FONT_SCALE_DEFAULT);
        assert_eq!(SIDEBAR_W.to_pixels(root_font_size()), px(256.));
    }

    /// 焦点环必须是三层（缝 + 实边 + 光晕），都在边框外面，光晕的 σ 是 CSS 值的一半。
    #[core::prelude::v1::test]
    fn focus_ring_has_three_layers() {
        let _g = theme_globals();
        set_palette(0);
        set_dark(false);
        let ring = focus_ring(CORAL);
        assert_eq!(ring.len(), 3);
        // 内阴影画在边框底下看不见，三层都得是外阴影
        assert!(ring.iter().all(|s| !s.inset));
        // ③ 光晕（最先画、在最底下）：CSS 的 6px 减半成 σ 3px，40% 透明度
        assert_eq!(ring[0].blur_radius, px(3.));
        assert!((ring[0].color.a - 0.4).abs() < 1e-4);
        // ② 实边：spread 2px、无模糊，里面那 1px 被缝盖住
        assert_eq!(ring[1].spread_radius, px(2.));
        assert_eq!(ring[1].blur_radius, px(0.));
        assert_eq!(ring[1].color, hsla_of(CORAL));
        // ① 缝（最后画、在最上面）：1px、无模糊、取元素外面的纸底
        assert_eq!(ring[2].spread_radius, px(1.));
        assert_eq!(ring[2].blur_radius, px(0.));
        assert_eq!(ring[2].color, hsla_of(PAPER));
        // 放在卡片里的元素换缝的颜色
        assert_eq!(focus_ring_on(CORAL, CARD)[2].color, hsla_of(CARD));
    }
}
