//! 公式排版（见「设计说明-聊天与Code分离.md」第六章）：LaTeX → 自包含 SVG。
//!
//! 只用 RaTeX 的四个能力：解析（`ratex-parser`）、布局（`ratex-layout`）、导出 SVG
//! （`ratex-svg`）。对外只有两件事：
//!
//! - [`normalize_markup`]：把模型爱写的 `\( … \)`、`\[ … \]`、裸 `equation` / `align` 环境
//!   统一成 `$…$` / `$$…$$`（代码块、行内代码里不动）；
//! - [`typeset`]：源码 → [`Typeset`]（SVG + 宽高深，单位 em），带缓存。
//!
//! 排版结果里字形是**路径**（`embed_glyphs`，KaTeX 字体打进程序），所以不依赖系统字体；
//! gpui 画 SVG 是单色蒙版（颜色在画的时候给），跟着 `INK`、外观和界面缩放进图集，不糊。

use ratex_layout::{LayoutOptions, layout, to_display_list, validate_layout};
use ratex_parser::parse as parse_latex;
use ratex_svg::{SvgOptions, render_to_svg};
use ratex_types::math_style::MathStyle;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 缓存最多记多少条（流式输出时整段回复每来一块都重新解析一次，前面的公式不该重排）。
const CACHE_MAX: usize = 512;

/// 一份排好的公式。
#[derive(Debug, Clone, PartialEq)]
pub struct Typeset {
    /// 自包含 SVG（字形是路径，颜色由画的时候给）
    pub svg: String,
    /// 宽（em，相对正文字号）
    pub width: f64,
    /// 基线以上（em）
    pub height: f64,
    /// 基线以下（em）
    pub depth: f64,
}

impl Typeset {
    /// 这块公式整个占的高度（em）。
    pub fn total_height(&self) -> f64 {
        self.height + self.depth
    }

    /// 宽高比（宽 / 总高），摆到界面上时按它算尺寸，免得拉伸变形。
    pub fn aspect(&self) -> f32 {
        let total = self.total_height();
        if total <= 0.0 || self.width <= 0.0 { 1. } else { (self.width / total) as f32 }
    }
}

/// 排版失败的原因（给用户看的中文；原文附在后面，方便看出来是哪条命令不认识）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathError {
    /// 中文说明 + RaTeX 的原文
    pub message: String,
}

impl MathError {
    fn new(cause: impl Into<String>) -> Self {
        let cause = cause.into();
        Self { message: format!("公式没能排版：{cause}") }
    }
}

impl std::fmt::Display for MathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MathError {}

/// 排版一段公式（`latex` 可以带 `$` / `$$`，会被剥掉）。
///
/// 结果按「源码 + 行内 / 块」缓存：同一个公式在流式输出里会来回排好几次，只有第一次真排。
pub fn typeset(latex: &str, display: bool) -> Result<Typeset, MathError> {
    let key = (latex.to_string(), display);
    if let Some(hit) = cache().lock().unwrap().get(&key) {
        return hit.clone();
    }
    let out = typeset_uncached(latex, display);
    let mut cache = cache().lock().unwrap();
    if cache.len() >= CACHE_MAX {
        // 满了清一半：新的一批接着进来，不必真做 LRU（公式不多，简单够了）
        cache.clear();
    }
    cache.insert(key, out.clone());
    out
}

/// 排版缓存：（源码, 是不是块公式）→ 结果
type Cache = Mutex<HashMap<(String, bool), Result<Typeset, MathError>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn typeset_uncached(latex: &str, display: bool) -> Result<Typeset, MathError> {
    let ast = parse_latex(latex).map_err(|e| MathError::new(e.message))?;
    let options = LayoutOptions {
        style: if display { MathStyle::Display } else { MathStyle::Text },
        ..LayoutOptions::default()
    };
    let root = layout(&ast, &options);
    // 几百行的矩阵这种：超了就不画，交回源码
    validate_layout(&root).map_err(MathError::new)?;
    let list = to_display_list(&root);
    let svg = render_to_svg(
        &list,
        &SvgOptions {
            // 一个单位 = 1 em：界面按字号乘上去，任何缩放都是矢量的
            font_size: 1.,
            padding: 0.,
            stroke_width: 0.06,
            // 字形转路径：不依赖系统字体，gpui 直接贴图集
            embed_glyphs: true,
            font_dir: String::new(),
        },
    );
    Ok(Typeset { svg, width: list.width, height: list.height, depth: list.depth })
}

// ---------- 写法归一 ----------

/// AMS 的显示环境：裸着写的时候要包进 `$$`（`\begin{equation}` / `\begin{align}` …）。
const DISPLAY_ENVS: &[&str] = &[
    "equation",
    "align",
    "alignat",
    "gather",
    "flalign",
    "multline",
    "eqnarray",
    "equation*",
    "align*",
    "alignat*",
    "gather*",
    "flalign*",
    "multline*",
    "eqnarray*",
];

/// 把模型爱写的几种公式写法统一成 `$…$` / `$$…$$`（`\( … \)`、`\[ … \]`、裸的显示环境）。
///
/// **只在代码块和行内代码之外改**：代码里出现的 `\(` 是正则、是转义，不能动。
/// 已经是 `$` / `$$` 的原样留着（里面不再改）。
pub fn normalize_markup(text: &str) -> std::borrow::Cow<'_, str> {
    if !needs_normalize(text) {
        return std::borrow::Cow::Borrowed(text);
    }
    // 先把代码（围栏块、行内代码）圈成受保护区：里面的 `\(` 是正则、是转义，一律不动；
    // 找收尾的 `\)` / `$` 也不许跨进保护区（半个公式配到代码里的反斜杠上就坏了）
    let protected = protected_ranges(text);
    let mut out = String::with_capacity(text.len() + 32);
    let mut i = 0;
    let mut next_protected = 0;
    while i < text.len() {
        // 进到保护区：整段原样抄过去
        if next_protected < protected.len() && i == protected[next_protected].0 {
            let (start, end) = protected[next_protected];
            out.push_str(&text[start..end]);
            i = end;
            next_protected += 1;
            continue;
        }
        let limit =
            if next_protected < protected.len() { protected[next_protected].0 } else { text.len() };
        let rest = &text[i..limit];
        let next = rest.chars().next();
        let Some(next) = next else {
            // 剩下的全是保护内容（上面那个分支会接住），兜底抄一个字符
            out.push_str(&text[i..(i + 1)]);
            i += 1;
            continue;
        };
        // 已经是 $ / $$ 的：原样抄到收尾的 $ 为止
        if next == '$' {
            let run = rest.bytes().take_while(|b| *b == b'$').count();
            let delim = &text[i..i + run.min(2)];
            let body_start = i + delim.len();
            match text[body_start..limit].find(delim) {
                Some(at) => {
                    let end = body_start + at + delim.len();
                    out.push_str(&text[i..end]);
                    i = end;
                }
                // 落单的 `$`（多半是价格）：只抄它自己，后面的 `\(` 照常转换
                None => {
                    out.push_str(delim);
                    i = body_start;
                }
            }
            continue;
        }
        // \( … \) → $…$
        if let Some(inner) = rest.strip_prefix("\\(") {
            match inner.find("\\)") {
                Some(at) => {
                    out.push('$');
                    out.push_str(&inner[..at]);
                    out.push('$');
                    i += 2 + at + 2;
                }
                None => {
                    out.push_str("\\(");
                    i += 2;
                }
            }
            continue;
        }
        // \[ … \] → $$ … $$（前后补空行，保证是块）
        if let Some(inner) = rest.strip_prefix("\\[") {
            match inner.find("\\]") {
                Some(at) => {
                    let body = inner[..at].trim();
                    push_block(&mut out, body);
                    i += 2 + at + 2;
                }
                None => {
                    out.push_str("\\[");
                    i += 2;
                }
            }
            continue;
        }
        // 裸的显示环境：包进 $$
        if rest.starts_with("\\begin{")
            && let Some((env, after)) = begin_env(&text[..limit], i)
            && DISPLAY_ENVS.contains(&env.as_str())
        {
            let end_tag = format!("\\end{{{env}}}");
            if let Some(at) = text[after..limit].find(&end_tag) {
                let body = &text[i..after + at + end_tag.len()];
                push_block(&mut out, body);
                i = after + at + end_tag.len();
                continue;
            }
        }
        out.push(next);
        i += next.len_utf8();
    }
    std::borrow::Cow::Owned(out)
}

/// 不能动的区域：围栏代码块（含围栏那一行）和行内代码（含反引号），按起点排序。
fn protected_ranges(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut fence: Option<(u8, usize, usize)> = None; // (字符, 长度, 起点)
    let mut line_start = 0usize;
    let mut i = 0usize;
    while i <= bytes.len() {
        let at_line_start = i == line_start;
        if at_line_start {
            let rest = &text[i..];
            let indent = rest.len() - rest.trim_start_matches([' ', '\t']).len();
            let after = &rest[indent..];
            let ch = after.as_bytes().first().copied().unwrap_or(b' ');
            if (ch == b'`' || ch == b'~') && indent <= 3 {
                let len = after.bytes().take_while(|b| *b == ch).count();
                match fence {
                    Some((f, n, start)) if f == ch && len >= n => {
                        let end = line_end_of(text, i);
                        ranges.push((start, end));
                        fence = None;
                        i = end;
                        line_start = i;
                        // 跳过换行
                        if i < bytes.len() {
                            i += 1;
                            line_start = i;
                        }
                        continue;
                    }
                    None if len >= 3 => {
                        // 行首的 ``` / ~~~：找它自己的收尾（也可能一直没闭合，那就护到结尾）
                        match find_fence_close(text, i, ch, len) {
                            Some(end) => {
                                ranges.push((i, end));
                                i = end;
                                line_start = i;
                                if i < bytes.len() {
                                    i += 1;
                                    line_start = i;
                                }
                                continue;
                            }
                            None => {
                                ranges.push((i, text.len()));
                                i = text.len();
                                continue;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // 行内代码（和行首那种分开：这里是行中出现的一串反引号）
        if i < bytes.len() && bytes[i] == b'`' && !at_line_start {
            let run = bytes[i..].iter().take_while(|b| **b == b'`').count();
            let end = find_backtick_run(text, i + run, run)
                .map(|at| at + run)
                .unwrap_or_else(|| line_end_of(text, i));
            ranges.push((i, end));
            i = end;
            continue;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'\n' {
            i += 1;
            line_start = i;
            continue;
        }
        i += 1;
    }
    ranges.sort_by_key(|r| r.0);
    ranges
}

/// 从行首的围栏 `at` 开始找它的收尾行，返回收尾行的结尾（不含换行）。
fn find_fence_close(text: &str, at: usize, ch: u8, len: usize) -> Option<usize> {
    let mut i = line_end_of(text, at);
    while i < text.len() {
        i += 1; // 跳过换行
        let rest = &text[i..];
        let indent = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let after = &rest[indent..];
        if after.as_bytes().first().copied() == Some(ch) {
            let n = after.bytes().take_while(|b| *b == ch).count();
            if n >= len {
                return Some(line_end_of(text, i));
            }
        }
        i = line_end_of(text, i);
    }
    None
}

fn needs_normalize(text: &str) -> bool {
    text.contains("\\(")
        || text.contains("\\[")
        || DISPLAY_ENVS.iter().any(|env| text.contains(env))
}

/// 块公式：前后各补一行空行，让它单独成块（`$` 里的内容不缩进，居中由界面管）。
fn push_block(out: &mut String, body: &str) {
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out.push('\n');
    out.push_str("$$\n");
    out.push_str(body.trim_matches('\n'));
    out.push_str("\n$$\n\n");
}

/// `\begin{env}` 之后的位置；不是环境返回 None。
fn begin_env(text: &str, at: usize) -> Option<(String, usize)> {
    let rest = text.get(at.checked_add(7)?..)?;
    let close = rest.find('}')?;
    let env = rest[..close].to_string();
    Some((env, at + 7 + close + 1))
}

/// 从 `at` 开始找 `run` 个反引号（行内代码的收尾），找不到返回 None。
fn find_backtick_run(text: &str, at: usize, run: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = at;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let n = bytes[i..].iter().take_while(|b| **b == b'`').count();
            if n == run {
                return Some(i);
            }
            i += n;
            continue;
        }
        if bytes[i] == b'\n' {
            return None;
        }
        i += 1;
    }
    None
}

/// 这一行的结尾（不含换行符）。
fn line_end_of(text: &str, at: usize) -> usize {
    match text[at..].find('\n') {
        Some(n) => at + n,
        None => text.len(),
    }
}

// ---------- 认领不认领（货币规则） ----------

/// 这个行内公式节点该不该认领（Pandoc 的规则）：开头的 `$` 后面不能是空白，
/// 结尾的 `$` 前面不能是空白、后面不能紧跟数字。
///
/// `花了 $5 和 $10` 不认领（交回 Markdown 当普通文字）；`$x$`、`$\alpha+1$` 认领。
pub fn claim_inline(source: &str) -> bool {
    let Some(inner) = source.strip_prefix('$').and_then(|s| s.strip_suffix('$')) else {
        return true;
    };
    if inner.starts_with(char::is_whitespace) || inner.ends_with(char::is_whitespace) {
        return false;
    }
    // 空的不认领（`$$` 是块，走到这里说明写坏了）
    !inner.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paren_and_bracket_delimiters() {
        assert_eq!(normalize_markup("设 $a$ 和 \\(b+c\\) 相等"), "设 $a$ 和 $b+c$ 相等");
        let out = normalize_markup("看这个：\n\\[\\frac{a}{b}\\]\n完了");
        assert!(out.contains("$$\n\\frac{a}{b}\n$$"), "{out}");
        // 前后补了空行，单独成块
        assert!(out.contains("\n\n$$\n"), "{out}");
    }

    #[test]
    fn leaves_code_alone() {
        let text =
            "正则 \\( 和 \\[ 要转义\n\n```js\nconst re = /\\(/\n```\n\n行内 `\\(x\\)` 也不动";
        let out = normalize_markup(text);
        assert!(out.contains("const re = /\\(/"), "{out}");
        assert!(out.contains("行内 `\\(x\\)` 也不动"), "{out}");
        // 代码块外面的 `\(` 前面没有收尾，原样留着（不是公式）
        assert!(out.contains("正则 \\( 和 \\[ 要转义"), "{out}");
    }

    #[test]
    fn wraps_bare_display_environments() {
        let out = normalize_markup("\\begin{align}\na &= b \\\\\nc &= d\n\\end{align}");
        assert!(out.trim_start().starts_with("$$"), "{out}");
        assert!(out.trim_end().ends_with("$$"), "{out}");
        assert!(out.contains("a &= b"), "{out}");
        // 行内的 aligned（不是显示环境）不包
        let inline = normalize_markup("$\\begin{aligned}a&=b\\end{aligned}$");
        assert_eq!(inline, "$\\begin{aligned}a&=b\\end{aligned}$");
        // 环境前面有正文时同样整段包进去（不能从环境中间截断）
        let after_text = normalize_markup("推导如下：\n\\begin{align}\na &= b\n\\end{align}\n完");
        assert!(after_text.contains("$$\n\\begin{align}"), "{after_text}");
        assert!(after_text.contains("\\end{align}\n$$"), "{after_text}");
    }

    #[test]
    fn does_not_touch_existing_dollar_math() {
        assert_eq!(normalize_markup("$x^2$ 和 $$\\frac{1}{2}$$"), "$x^2$ 和 $$\\frac{1}{2}$$");
        // 没有要改的直接借用原文
        assert!(matches!(normalize_markup("纯文字"), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn unclosed_backslash_paren_survives_streaming() {
        // 流式中途只写了一半：原样留着，等收尾
        assert_eq!(normalize_markup("结果是 \\(x + 1"), "结果是 \\(x + 1");
    }

    #[test]
    fn a_lone_dollar_does_not_stop_normalizing() {
        assert_eq!(normalize_markup("costs $5. Formula: \\(x^2\\)"), "costs $5. Formula: $x^2$");
        assert_eq!(normalize_markup("$$ open \\(a\\)"), "$$ open $a$");
    }

    #[test]
    fn currency_rule() {
        assert!(!claim_inline("$5 和 $"));
        assert!(!claim_inline("$ x$"));
        assert!(!claim_inline("$x $"));
        assert!(claim_inline("$x^2$"));
        assert!(claim_inline("$\\alpha+1$"));
        // 不是一对 $（调用方传错）时不拦
        assert!(claim_inline("\\frac{1}{2}"));
    }

    #[test]
    fn typesets_a_fraction() {
        let t = typeset("\\frac{-b \\pm \\sqrt{b^2-4ac}}{2a}", true).unwrap();
        assert!(t.svg.starts_with("<svg"), "{}", t.svg);
        assert!(t.width > 0.5, "{t:?}");
        assert!(t.height > 0.5, "{t:?}");
        // 字形转成了路径，不靠 <text> 找系统字体
        assert!(t.svg.contains("<path"), "{}", t.svg);
        assert!(!t.svg.contains("<text"), "{}", t.svg);
        // 缓存命中：第二次是同一份
        assert_eq!(typeset("\\frac{-b \\pm \\sqrt{b^2-4ac}}{2a}", true).unwrap(), t);
    }

    #[test]
    fn typesets_the_common_shapes() {
        for src in [
            "\\sqrt{x}",
            "\\int_0^\\infty e^{-x^2} dx = \\frac{\\sqrt{\\pi}}{2}",
            "\\begin{pmatrix}a & b \\\\ c & d\\end{pmatrix}",
            "\\begin{align}a &= b \\\\ c &= d\\end{align}",
            "\\ce{H2SO4 + 2NaOH -> Na2SO4 + 2H2O}",
            "x_i^2 \\le \\sum_{j=1}^n y_j",
        ] {
            let t = typeset(src, true).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert!(t.width > 0. && t.height + t.depth > 0., "{src}: {t:?}");
        }
    }

    #[test]
    fn syntax_errors_are_reported_not_panicked() {
        let err = typeset("\\frac{1}{", true).unwrap_err();
        assert!(err.message.contains("公式没能排版"), "{err}");
        // 不认识的命令也一样
        assert!(typeset("\\notarealcommand{x}", false).is_err());
    }

    #[test]
    fn inline_and_display_differ_in_size() {
        // 同一个分式：块级用 Display 样式（分子分母不缩小），行内用 Text 样式，宽高不一样
        let inline = typeset("\\frac{1}{2}", false).unwrap();
        let display = typeset("\\frac{1}{2}", true).unwrap();
        assert!(display.total_height() > inline.total_height());
    }
}
