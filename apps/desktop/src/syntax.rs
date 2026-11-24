//! 代码块语法高亮配色方案、缓存与常见语言别名登记。
//!
//! 每套代码配色都有浅色 / 深色两版，写入 `gpui-kit` 的 `Theme::highlight_theme`，
//! 由 `TextViewDefaults` 自动应用到聊天流里的所有 Markdown 代码块。

use crate::settings::CodeThemeChoice;
use crate::theme::Shade;
use gpui_kit::component::highlighter::{HighlightTheme, LanguageRegistry};
use gpui_kit::component::theme::ThemeMode;
use gpui_kit::{Rgba, rgb};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once, OnceLock};

const fn s(light: u32, dark: u32) -> Shade {
    Shade { light, dark }
}

/// 一套语法高亮配色（全部语义角色都带浅 / 深两档取值）。
#[derive(Clone, Copy)]
struct SyntaxPalette {
    keyword: Shade,
    function: Shade,
    type_: Shade,
    string: Shade,
    string_special: Shade,
    number: Shade,
    property: Shade,
    variable_special: Shade,
    tag: Shade,
    operator: Shade,
    comment: Shade,
    punctuation: Shade,
    title: Shade,
}

impl SyntaxPalette {
    const fn pick(self, shade: Shade, dark: bool) -> u32 {
        if dark { shade.dark } else { shade.light }
    }

    fn swatches(self, dark: bool) -> [Rgba; 5] {
        [
            rgb(self.pick(self.keyword, dark)),
            rgb(self.pick(self.function, dark)),
            rgb(self.pick(self.string, dark)),
            rgb(self.pick(self.type_, dark)),
            rgb(self.pick(self.number, dark)),
        ]
    }
}

/// 顺序与 `CodeThemeChoice`（去掉 `Follow` 后的 9 个枚举项）一一对应。
const SCHEMES: [(&str, SyntaxPalette); 9] = [
    (
        "GitHub",
        SyntaxPalette {
            keyword: s(0xCF222E, 0xFF7B72),
            function: s(0x8250DF, 0xD2A8FF),
            type_: s(0x953800, 0xFFA657),
            string: s(0x0A3069, 0xA5D6FF),
            string_special: s(0x116329, 0x7EE787),
            number: s(0x0550AE, 0x79C0FF),
            property: s(0x0550AE, 0x79C0FF),
            variable_special: s(0x953800, 0xFFA657),
            tag: s(0x116329, 0x7EE787),
            operator: s(0xCF222E, 0xFF7B72),
            comment: s(0x57606A, 0x8B949E),
            punctuation: s(0x57606A, 0x9198A1),
            title: s(0x0550AE, 0x79C0FF),
        },
    ),
    (
        "One",
        SyntaxPalette {
            keyword: s(0xA626A4, 0xC678DD),
            function: s(0x3068C8, 0x61AFEF),
            type_: s(0x906100, 0xE5C07B),
            string: s(0x357530, 0x98C379),
            string_special: s(0x017488, 0x56B6C2),
            number: s(0x906100, 0xD19A66),
            property: s(0xC93B32, 0xE06C75),
            variable_special: s(0xC93B32, 0xE06C75),
            tag: s(0xC93B32, 0xE06C75),
            operator: s(0x017488, 0x56B6C2),
            comment: s(0x696C77, 0x8A93A2),
            punctuation: s(0x696C77, 0x969FA8),
            title: s(0xC93B32, 0xE06C75),
        },
    ),
    (
        "Catppuccin",
        SyntaxPalette {
            keyword: s(0x8839EF, 0xCBA6F7),
            function: s(0x1E66F5, 0x89B4FA),
            type_: s(0x925B00, 0xF9E2AF),
            string: s(0x2F761F, 0xA6E3A1),
            string_special: s(0xB43086, 0xF5C2E7),
            number: s(0xB84405, 0xFAB387),
            property: s(0x177B83, 0x94E2D5),
            variable_special: s(0xC71033, 0xF38BA8),
            tag: s(0x1E66F5, 0x89B4FA),
            operator: s(0x04789C, 0x89DCEB),
            comment: s(0x6C6F85, 0x9399B2),
            punctuation: s(0x6C6F85, 0x9399B2),
            title: s(0x1E66F5, 0x89B4FA),
        },
    ),
    (
        "Tokyo Night",
        SyntaxPalette {
            keyword: s(0x7847BD, 0xBB9AF7),
            function: s(0x2E5FB8, 0x7AA2F7),
            type_: s(0x187382, 0x2AC3DE),
            string: s(0x4A6D22, 0x9ECE6A),
            string_special: s(0x117572, 0x73DACA),
            number: s(0xA44D00, 0xFF9E64),
            property: s(0x117572, 0x73DACA),
            variable_special: s(0xC22D48, 0xF7768E),
            tag: s(0xC22D48, 0xF7768E),
            operator: s(0x006A83, 0x89DDFF),
            comment: s(0x616B85, 0x8690B8),
            punctuation: s(0x616B85, 0x8994BA),
            title: s(0x2E5FB8, 0x7AA2F7),
        },
    ),
    (
        "Rosé Pine",
        SyntaxPalette {
            keyword: s(0x286983, 0x5FB2D4),
            function: s(0xA95550, 0xEA9A97),
            type_: s(0x3B727A, 0x9CCFD8),
            string: s(0x8F5C14, 0xF6C177),
            string_special: s(0x7B5B9C, 0xC4A7E7),
            number: s(0xB44967, 0xEB6F92),
            property: s(0x3B727A, 0x9CCFD8),
            variable_special: s(0xB44967, 0xEB6F92),
            tag: s(0x286983, 0x9CCFD8),
            operator: s(0x575279, 0x9CCFD8),
            comment: s(0x6E6A86, 0x908CAA),
            punctuation: s(0x6E6A86, 0x908CAA),
            title: s(0x7B5B9C, 0xC4A7E7),
        },
    ),
    (
        "Everforest",
        SyntaxPalette {
            keyword: s(0xBC3D40, 0xE67E80),
            function: s(0x537317, 0xA7C080),
            type_: s(0x8E6000, 0xDBBC7F),
            string: s(0x2B7A5E, 0x83C092),
            string_special: s(0xA8501A, 0xE69875),
            number: s(0xA64B8A, 0xD699B6),
            property: s(0x2D6D87, 0x7FBBB3),
            variable_special: s(0xA64B8A, 0xD699B6),
            tag: s(0xA8501A, 0xE69875),
            operator: s(0xA8501A, 0xE69875),
            comment: s(0x6A7569, 0x8C9B8F),
            punctuation: s(0x6A7569, 0x95A398),
            title: s(0x537317, 0xA7C080),
        },
    ),
    (
        "Nord",
        SyntaxPalette {
            keyword: s(0x3B5E8C, 0x81A1C1),
            function: s(0x246F84, 0x88C0D0),
            type_: s(0x23756F, 0x8FBCBB),
            string: s(0x4E7334, 0xA3BE8C),
            string_special: s(0x8F5C11, 0xEBCB8B),
            number: s(0x884F82, 0xB48EAD),
            property: s(0x3B5E8C, 0x88C0D0),
            variable_special: s(0xAD454F, 0xDF7A84),
            tag: s(0x3B5E8C, 0x81A1C1),
            operator: s(0x3B5E8C, 0x81A1C1),
            comment: s(0x5B667A, 0x8C97AB),
            punctuation: s(0x5B667A, 0x96A1B5),
            title: s(0x246F84, 0x88C0D0),
        },
    ),
    (
        "Flexoki",
        SyntaxPalette {
            keyword: s(0x5C7800, 0x95AD45),
            function: s(0xB04E0D, 0xE3823E),
            type_: s(0x8F6400, 0xDFB32A),
            string: s(0x1E766E, 0x4CB8AE),
            string_special: s(0xA02F6F, 0xDE73AA),
            number: s(0x5E409D, 0xA487E0),
            property: s(0x205EA6, 0x62A1EB),
            variable_special: s(0xAF3029, 0xE86D66),
            tag: s(0x205EA6, 0x62A1EB),
            operator: s(0x575653, 0x9F9D96),
            comment: s(0x6F6E69, 0x94928B),
            punctuation: s(0x6F6E69, 0x94928B),
            title: s(0x205EA6, 0x62A1EB),
        },
    ),
    (
        "Gruvbox",
        SyntaxPalette {
            keyword: s(0x9D0006, 0xFB4934),
            function: s(0x68720C, 0xB8BB26),
            type_: s(0x9B620A, 0xFABD2F),
            string: s(0x3B6E4F, 0x8EC07C),
            string_special: s(0xAF3A03, 0xFE8019),
            number: s(0x8F3F71, 0xD3869B),
            property: s(0x076678, 0x83A598),
            variable_special: s(0xAF3A03, 0xFE8019),
            tag: s(0x076678, 0x83A598),
            operator: s(0xAF3A03, 0xFE8019),
            comment: s(0x6D635B, 0x9D9186),
            punctuation: s(0x6D635B, 0xA89984),
            title: s(0x68720C, 0xB8BB26),
        },
    ),
];

static CODE_THEME: AtomicUsize = AtomicUsize::new(0);

/// 切换代码配色（之后要经 `Theme::update` 重写组件主题并重绘）。
pub fn set_code_theme(choice: CodeThemeChoice) {
    CODE_THEME.store(choice.index(), Ordering::Relaxed);
}

/// 当前生效的代码配色枚举项。
pub fn code_theme() -> CodeThemeChoice {
    let idx = CODE_THEME.load(Ordering::Relaxed);
    CodeThemeChoice::ALL.get(idx).copied().unwrap_or_default()
}

/// 把 `CodeThemeChoice`（含「跟随界面配色」）解析到 `SCHEMES` 下标。
fn resolve_scheme(choice: CodeThemeChoice, ui_palette_index: usize) -> usize {
    match choice {
        CodeThemeChoice::Follow => match ui_palette_index {
            0 => 0, // Kimi -> GitHub
            1 => 7, // Warm -> Flexoki
            2 => 7, // Flexoki -> Flexoki
            3 => 4, // RosePine -> Rosé Pine
            4 => 5, // Everforest -> Everforest
            5 => 6, // Nord -> Nord
            6 => 2, // Catppuccin -> Catppuccin
            7 => 0, // Github -> GitHub
            8 => 0, // Pure -> GitHub
            9 => 3, // Lavender -> Tokyo Night
            _ => 0,
        },
        other => other.index().saturating_sub(1).min(SCHEMES.len() - 1),
    }
}

fn hex_str(v: u32) -> String {
    format!("#{v:06x}")
}

/// 从一套语法色板构造 `gpui-component` 使用的 `HighlightTheme`。
///
/// 不给 `comment` 加斜体：内嵌的 JetBrains Mono 只带 Regular/Medium，没有斜体字形；
/// 不给 `variable` / `embedded` / `primary` 设颜色，让普通标识符自然继承当前界面配色的 `INK`。
fn build_highlight_theme(name: &str, pal: &SyntaxPalette, dark: bool) -> Arc<HighlightTheme> {
    let appearance = if dark { ThemeMode::Dark } else { ThemeMode::Light };
    let mode_label = if dark { "Dark" } else { "Light" };
    // 注意：gpui-component 的 `SyntaxColors::comment_doc` 没有 `#[serde(rename = "comment.doc")]`，
    // 反序列化时必须写 `"comment_doc"` 才能落到该字段。
    let tokens = [
        ("attribute", pal.property),
        ("boolean", pal.number),
        ("comment", pal.comment),
        ("comment_doc", pal.comment),
        ("constant", pal.number),
        ("constructor", pal.type_),
        ("enum", pal.type_),
        ("function", pal.function),
        ("hint", pal.comment),
        ("keyword", pal.keyword),
        ("label", pal.string_special),
        ("link_text", pal.function),
        ("link_uri", pal.string),
        ("number", pal.number),
        ("operator", pal.operator),
        ("predictive", pal.comment),
        ("preproc", pal.keyword),
        ("property", pal.property),
        ("punctuation", pal.punctuation),
        ("punctuation.bracket", pal.punctuation),
        ("punctuation.delimiter", pal.punctuation),
        ("punctuation.list_marker", pal.operator),
        ("punctuation.special", pal.string_special),
        ("string", pal.string),
        ("string.escape", pal.string_special),
        ("string.regex", pal.string_special),
        ("string.special", pal.string_special),
        ("string.special.symbol", pal.string_special),
        ("tag", pal.tag),
        ("tag.doctype", pal.keyword),
        ("text.code.span", pal.string),
        ("text.literal", pal.string),
        ("title", pal.title),
        ("type", pal.type_),
        ("variable.special", pal.variable_special),
        ("variant", pal.type_),
    ];
    let mut syntax = serde_json::Map::with_capacity(tokens.len() + 2);
    for (key, shade) in tokens {
        syntax.insert(key.into(), serde_json::json!({ "color": hex_str(pal.pick(shade, dark)) }));
    }
    syntax.insert("emphasis".into(), serde_json::json!({ "font_style": "italic" }));
    syntax.insert("emphasis.strong".into(), serde_json::json!({ "font_weight": 700 }));

    let value = serde_json::json!({
        "name": format!("{name} {mode_label}"),
        "appearance": appearance,
        "style": {
            "syntax": serde_json::Value::Object(syntax),
        }
    });
    Arc::new(serde_json::from_value(value).expect("内置语法高亮主题 JSON 格式合法"))
}

fn cached_themes() -> &'static [Arc<HighlightTheme>] {
    static CACHE: OnceLock<Vec<Arc<HighlightTheme>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut list = Vec::with_capacity(SCHEMES.len() * 2);
        for (name, pal) in &SCHEMES {
            list.push(build_highlight_theme(name, pal, false));
            list.push(build_highlight_theme(name, pal, true));
        }
        list
    })
}

/// 按当前设置的代码配色与界面明暗取出缓存的 `HighlightTheme`。
pub fn active_highlight_theme(ui_palette: usize, dark: bool) -> Arc<HighlightTheme> {
    highlight_theme(code_theme(), ui_palette, dark)
}

/// 按指定代码配色、界面配色下标与明暗取出缓存的 `HighlightTheme`。
pub fn highlight_theme(
    choice: CodeThemeChoice,
    ui_palette: usize,
    dark: bool,
) -> Arc<HighlightTheme> {
    let scheme = resolve_scheme(choice, ui_palette);
    let idx = scheme * 2 + usize::from(dark);
    cached_themes()[idx].clone()
}

/// 代码配色菜单条目底部展示的 5 个代表色块（关键字 / 函数 / 字符串 / 类型 / 数字）。
pub fn swatches(choice: CodeThemeChoice, ui_palette: usize, dark: bool) -> [Rgba; 5] {
    let scheme = resolve_scheme(choice, ui_palette);
    SCHEMES[scheme].1.swatches(dark)
}

/// 为 `LanguageRegistry` 补齐模型输出常见的大小写与别名（如 `shell` / `jsx` / `Python`）。
///
/// 保持各配置自身的规范名称不变，让线程本地 `SyntaxHighlighter` 缓存可以继续按规范名称命中。
pub fn register_language_aliases() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        const ALIASES: &[(&str, &[&str])] = &[
            ("bash", &["shell", "zsh", "console", "shellsession", "Bash", "Shell", "SH"]),
            ("tsx", &["jsx", "TSX", "JSX"]),
            ("javascript", &["mjs", "cjs", "JavaScript", "Javascript", "JS"]),
            ("typescript", &["mts", "cts", "TypeScript", "Typescript", "TS"]),
            ("python", &["py3", "python3", "Python", "PY"]),
            ("rust", &["Rust", "RS"]),
            ("go", &["golang", "Go", "GO"]),
            ("c", &["h", "C"]),
            ("cpp", &["cxx", "cc", "hpp", "hh", "hxx", "C++", "CPP", "Cpp"]),
            ("java", &["Java"]),
            ("kotlin", &["Kotlin"]),
            ("json", &["json5", "JSON", "JSONC"]),
            ("yaml", &["YAML", "YML"]),
            ("toml", &["TOML"]),
            ("html", &["xml", "svg", "HTML", "XML", "SVG"]),
            ("css", &["CSS", "SCSS"]),
            ("sql", &["mysql", "postgres", "postgresql", "sqlite", "SQL"]),
            ("markdown", &["gfm", "Markdown", "MD"]),
            ("diff", &["patch", "Diff"]),
            ("ruby", &["Ruby"]),
            ("php", &["PHP"]),
            ("lua", &["Lua"]),
            ("zig", &["Zig"]),
            ("make", &["Makefile", "Make"]),
        ];
        let registry = LanguageRegistry::singleton();
        // gpui-component 构造 `cpp` 时只传了 `tree_sitter_cpp::HIGHLIGHT_QUERY`（仅含 C++ 增量节点），
        // 漏了基类 C 的关键字、数字、字符串、注释。把 `c` 的查询拼在前面，C++ 代码块才能完整着色。
        if let (Some(c_cfg), Some(mut cpp_cfg)) = (registry.language("c"), registry.language("cpp"))
            && !c_cfg.highlights.is_empty()
        {
            cpp_cfg.highlights = format!("{}\n{}", c_cfg.highlights, cpp_cfg.highlights).into();
            registry.register("cpp", &cpp_cfg);
        }
        for &(canonical, aliases) in ALIASES {
            let Some(config) = registry.language(canonical) else {
                continue;
            };
            for &alias in aliases {
                if registry.language(alias).is_none() {
                    registry.register(alias, &config);
                }
            }
        }
        // `c++` 在 `Language::from_name` 中也会退回 `cpp`，显式登记一份以沿用合并后的查询。
        if let Some(cpp_cfg) = registry.language("cpp") {
            registry.register("c++", &cpp_cfg);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ThemeChoice;
    use gpui_kit::component::Rope;
    use gpui_kit::component::highlighter::SyntaxHighlighter;

    #[core::prelude::v1::test]
    fn all_schemes_build_valid_highlight_themes() {
        let required = [
            "keyword",
            "function",
            "type",
            "string",
            "string.escape",
            "number",
            "boolean",
            "constant",
            "property",
            "tag",
            "operator",
            "comment",
            "comment.doc",
            "punctuation",
            "title",
        ];
        for (i, choice) in CodeThemeChoice::ALL.into_iter().skip(1).enumerate() {
            assert_eq!(resolve_scheme(choice, 0), i);
            for dark in [false, true] {
                let theme = highlight_theme(choice, 0, dark);
                assert_eq!(theme.appearance.is_dark(), dark);
                for key in required {
                    assert!(
                        theme.style(key).and_then(|s| s.color).is_some(),
                        "{choice:?} ({dark}) 缺少 {key}"
                    );
                }
                assert!(
                    theme.style("variable").and_then(|s| s.color).is_none(),
                    "{choice:?} 不应覆盖普通 variable 颜色"
                );
            }
        }
    }

    #[core::prelude::v1::test]
    fn follow_resolves_every_ui_palette() {
        for palette in ThemeChoice::ALL {
            let light = highlight_theme(CodeThemeChoice::Follow, palette.index(), false);
            let dark = highlight_theme(CodeThemeChoice::Follow, palette.index(), true);
            assert!(!light.appearance.is_dark());
            assert!(dark.appearance.is_dark());
            assert_ne!(
                light.style("keyword").and_then(|s| s.color),
                dark.style("keyword").and_then(|s| s.color),
                "{palette:?} 下跟随配色应当区分浅深外观"
            );
        }
    }

    #[core::prelude::v1::test]
    fn syntax_contrast() {
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

        let mut failures = Vec::new();
        for (scheme_name, pal) in &SCHEMES {
            let primary = [
                ("keyword", pal.keyword),
                ("function", pal.function),
                ("type", pal.type_),
                ("string", pal.string),
                ("string_special", pal.string_special),
                ("number", pal.number),
                ("property", pal.property),
                ("variable_special", pal.variable_special),
                ("tag", pal.tag),
                ("operator", pal.operator),
                ("title", pal.title),
            ];
            let muted = [("comment", pal.comment), ("punctuation", pal.punctuation)];

            for dark in [false, true] {
                for (ui_idx, ui) in ThemeChoice::ALL.iter().enumerate() {
                    let bg = crate::theme::sand_hex(ui_idx, dark);
                    for (token, shade) in primary {
                        let min = if ui_idx == 0 { 4.5 } else { 4.0 };
                        let r = ratio(pal.pick(shade, dark), bg);
                        if r < min {
                            failures.push(format!(
                                "代码配色 {scheme_name}（{}）{token} 在界面色板 {ui:?} 上对比度只有 {r:.2}:1（要求 ≥ {min:.1}:1）",
                                if dark { "深色" } else { "浅色" }
                            ));
                        }
                    }
                    for (token, shade) in muted {
                        let r = ratio(pal.pick(shade, dark), bg);
                        if r < 3.0 {
                            failures.push(format!(
                                "代码配色 {scheme_name}（{}）{token} 在界面色板 {ui:?} 上对比度只有 {r:.2}:1（要求 ≥ 3.0:1）",
                                if dark { "深色" } else { "浅色" }
                            ));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[core::prelude::v1::test]
    fn tree_sitter_highlights_languages_and_switches_themes() {
        register_language_aliases();

        let samples = [
            ("rust", "fn main() {\n    let x = 42;\n    println!(\"{x}\");\n}"),
            ("Rust", "pub struct Point { pub x: i32 }"),
            ("python", "def greet(name: str) -> int:\n    return len(name) + 1"),
            ("typescript", "interface User { id: number }\nexport const n: number = 1;"),
            ("javascript", "function add(a, b) { return a + b; }"),
            ("jsx", "export function App() { return <div className=\"ok\">hi</div>; }"),
            ("json", "{\"ok\": true, \"count\": 12}"),
            ("bash", "if [ -f Cargo.toml ]; then echo \"ok\"; fi"),
            ("shell", "export RUST_LOG=info && cargo test"),
            ("go", "package main\nfunc main() { println(42) }"),
            ("sql", "SELECT id, name FROM users WHERE active = 1;"),
            ("html", "<div class=\"card\"><span>hello</span></div>"),
            ("css", ".card { color: #1783ff; padding: 8px; }"),
            ("yaml", "name: claudesktop\nversion: 18"),
            ("toml", "[package]\nname = \"claudesktop\"\nedition = \"2024\""),
            ("diff", "--- a/foo.rs\n+++ b/foo.rs\n@@ -1 +1 @@\n-old\n+new"),
            ("c", "int main(void) { return 0; }"),
            ("cpp", "struct Box { int v = 1; };"),
        ];

        let github = highlight_theme(CodeThemeChoice::Github, 0, false);
        let catppuccin = highlight_theme(CodeThemeChoice::Catppuccin, 0, false);

        for (lang, code) in samples {
            let mut hl = SyntaxHighlighter::new(lang);
            let rope = Rope::from_str(code);
            assert!(hl.update(None, &rope, None), "解析 {lang} 超时");
            let styles = hl.styles(&(0..code.len()), github.as_ref());
            assert!(
                styles.iter().any(|(_, s)| s.color.is_some()),
                "语言 {lang} 没有任何带颜色的语法范围：{styles:?}"
            );
        }

        let rust_code = "fn main() { let count = 1; }";
        let mut hl = SyntaxHighlighter::new("rust");
        let rope = Rope::from_str(rust_code);
        hl.update(None, &rope, None);
        let fn_range = 0..2;
        let gh_styles = hl.styles(&fn_range, github.as_ref());
        let cat_styles = hl.styles(&fn_range, catppuccin.as_ref());
        let gh_color = gh_styles.iter().find_map(|(_, s)| s.color);
        let cat_color = cat_styles.iter().find_map(|(_, s)| s.color);
        assert!(gh_color.is_some() && cat_color.is_some());
        assert_ne!(gh_color, cat_color, "切换代码配色后关键字颜色应变化");
    }
}
