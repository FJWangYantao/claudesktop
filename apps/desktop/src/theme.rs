//! 色板与字体（Claude 品牌基准：暖纸底 + 赤陶橙）。

pub const PAPER: u32 = 0xFAF9F5;
pub const SAND: u32 = 0xF0EEE6;
pub const CORAL: u32 = 0xD97757;
pub const SAND_ACTIVE: u32 = 0xE8E3D6;
pub const INK: u32 = 0x3D3929;
pub const INK_SOFT: u32 = 0x87847C;
pub const INK_FAINT: u32 = 0xB5B0A4;
pub const LINE: u32 = 0xE5E1D5;
pub const CARD: u32 = 0xFFFFFF;
pub const GREEN: u32 = 0x7A9A65;
pub const RED: u32 = 0xC0504D;
pub const BLUE: u32 = 0x5B6FA8;
pub const BLUE_TINT: u32 = 0xE9EEF7;
// 字体：官方是自家的 Sans / Serif / Mono，无法分发，取 Windows 自带的近似款。
pub const SANS: &str = "Segoe UI Variable Text";
pub const MONO: &str = "Cascadia Mono";
pub const DIFF_ADD_BG: u32 = 0xE4F3DE;
pub const DIFF_ADD_FG: u32 = 0x3C6B2E;
pub const DIFF_DEL_BG: u32 = 0xFBE3DF;
pub const DIFF_DEL_FG: u32 = 0xA4443A;
// Claude 的标题使用衬线体；Windows 自带 Georgia 作近似，中文回退到系统衬线。
pub const SERIF: &str = "Georgia";
