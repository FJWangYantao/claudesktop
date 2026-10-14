//! 色板与字体（Claude 品牌基准：暖纸底 + 赤陶橙）。

pub const PAPER: u32 = 0xFAF9F5;
pub const SAND: u32 = 0xF0EEE6;
pub const CORAL: u32 = 0xD97757;
pub const SAND_ACTIVE: u32 = 0xE8E3D6;
// 文字色取自原型（更深的墨色，衬线正文更清晰）
pub const INK: u32 = 0x1F1E1D;
pub const INK_SOFT: u32 = 0x6E6B65;
pub const INK_FAINT: u32 = 0xB5B0A4;
pub const LINE: u32 = 0xE5E1D5;
pub const CARD: u32 = 0xFFFFFF;
pub const GREEN: u32 = 0x7A9A65;
pub const RED: u32 = 0xC0504D;
pub const BLUE: u32 = 0x5B6FA8;
pub const BLUE_TINT: u32 = 0xE9EEF7;
// 字体：官方的 Styrene / Tiempos 无法分发，用原型选的 OFL 近似款，内嵌见 `fonts.rs`。
pub const SANS: &str = "Inter";
pub const MONO: &str = "JetBrains Mono";
pub const DIFF_ADD_BG: u32 = 0xE4F3DE;
pub const DIFF_ADD_FG: u32 = 0x3C6B2E;
pub const DIFF_DEL_BG: u32 = 0xFBE3DF;
pub const DIFF_DEL_FG: u32 = 0xA4443A;
// 衬线：回复正文与标题（Newsreader，近似 Tiempos）；family 名是静态化后的命名
pub const SERIF: &str = "Newsreader 16pt 16pt";
