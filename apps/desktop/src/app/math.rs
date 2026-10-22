//! 公式的 Markdown 插件（见「设计说明-聊天与Code分离.md」第六章）。
//!
//! 排版在 `MarkdownPlugin::parse` 里做（gpui-kit 在后台线程解析 Markdown，不占 render），
//! 结果（自包含 SVG + 宽高深）放进 `MarkdownNode`，画的时候只是贴图。
//! 颜色不写死：gpui 画 SVG 是单色蒙版 + 着色，跟着 `INK`、外观和配色走。
//!
//! 认领规则：行内公式按 Pandoc（`$5 和 $10` 不认领，交回普通文字）；语法错的、
//! 流式中途还没写完的都不认领，原样显示源码（绝不因为一个错公式把整段回复弄坏）。

use crate::theme::rgb;
use claudesktop_core::math::{self, Typeset};
use gpui_kit::base::markdown_ast;
use gpui_kit::base::{
    InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode, MarkdownParseContext,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

/// 行内公式的插件名。
pub const INLINE_MATH: &str = "claudesktop-math-inline";
/// 块公式的插件名。
pub const BLOCK_MATH: &str = "claudesktop-math-block";

/// 行内公式相对正文的倍数（KaTeX 默认 1.21，配无衬线正文偏大）。
const INLINE_SCALE: f32 = 1.05;
/// 块公式相对正文的倍数。
const BLOCK_SCALE: f32 = 1.15;

/// 排版好的公式（放进 `MarkdownNode` 的数据里）。
#[derive(Clone, PartialEq)]
pub struct Formula {
    pub typeset: Typeset,
    /// 源码（复制、悬停提示都用它）
    pub source: String,
}

/// 行内公式：`$…$`，排好之后按基线嵌在正文里。
pub struct InlineMath;

impl gpui_kit::base::MarkdownPlugin for InlineMath {
    fn name(&self) -> &str {
        INLINE_MATH
    }

    fn parse(
        &self,
        node: &markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let markdown_ast::Node::InlineMath(math_node) = node else {
            return None;
        };
        let source = cx.node_source(node).unwrap_or(&math_node.value);
        // 货币和普通美元符号（`花了 $5 和 $10`）交给 Markdown 当普通文字
        if !math::claim_inline(source) {
            return None;
        }
        let typeset = math::typeset(&math_node.value, false).ok()?;
        claimed(INLINE_MATH, typeset, source)
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<InlineElement> {
        let formula = node.data::<Formula>()?;
        Some(inline_formula(formula, context))
    }
}

/// 块公式：`$$…$$`，居中单独一块。
pub struct BlockMath;

impl gpui_kit::base::MarkdownPlugin for BlockMath {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        BLOCK_MATH
    }

    fn parse(
        &self,
        node: &markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let markdown_ast::Node::Math(math_node) = node else {
            return None;
        };
        let source = cx.node_source(node).unwrap_or(&math_node.value);
        let typeset = math::typeset(&math_node.value, true).ok()?;
        claimed(BLOCK_MATH, typeset, source)
    }

    fn render(&self, node: &MarkdownNode, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let formula = node.data::<Formula>().cloned();
        let Some(formula) = formula else {
            return div().child(node.as_text().to_string()).into_any_element();
        };
        block_formula(&formula)
    }
}

/// 认领这个公式节点：把 SVG 登记进资源表，连源码一起放进节点数据。
/// `text` 是纯文本（选中复制、无障碍名字），`markdown` 是源码（导出 / 复制都还是 `$…$`）。
fn claimed(name: &str, typeset: Typeset, source: &str) -> Option<MarkdownNode> {
    if typeset.width <= 0. || typeset.total_height() <= 0. {
        return None;
    }
    crate::assets::register_math(&typeset.svg);
    Some(
        MarkdownNode::new(name, Formula { typeset, source: source.to_string() })
            .text(source.to_string())
            .markdown(source.to_string()),
    )
}

/// 行内公式：按基线对齐的 SVG，宽高跟着正文字号换算。
fn inline_formula(formula: &Formula, context: &InlineRenderContext) -> InlineElement {
    let em = f32::from(context.font_size()) * INLINE_SCALE;
    let width = px(em * formula.typeset.width as f32);
    let height = px(em * formula.typeset.total_height() as f32);
    let baseline = px(em * formula.typeset.height as f32);
    let element = svg()
        .path(math_path(formula))
        .w(width)
        .h(height)
        .flex_none()
        .text_color(rgb(crate::theme::INK));
    InlineElement::new(element).with_baseline(baseline)
}

/// 块公式：居中；比阅读列宽的横向滚动（不缩小，缩了字就糊了）。
fn block_formula(formula: &Formula) -> AnyElement {
    let em =
        f32::from(crate::theme::TEXT_LARGE.to_pixels(crate::theme::root_font_size())) * BLOCK_SCALE;
    let width = px(em * formula.typeset.width as f32);
    let height = px(em * formula.typeset.total_height() as f32);
    let source = formula.source.clone();
    let svg_el = svg().path(math_path(formula)).w(width).h(height);
    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .py_1()
        .items_center()
        .child(
            div()
                .id(ElementId::Name(format!("math-block-{}", hash_of(&formula.typeset.svg)).into()))
                .w_full()
                .min_w_0()
                .overflow_x_scroll()
                // 悬停看源码（`\tag`、`\color` 这类没画出来的东西还能照着抄）
                .tooltip(move |window, cx| super::chat::text_tip(&source, window, cx))
                .child(
                    div()
                        .flex()
                        .justify_center()
                        .min_w_full()
                        .text_color(rgb(crate::theme::INK))
                        .child(svg_el),
                ),
        )
        .into_any_element()
}

/// 资源名：`math/<内容哈希>.svg`（和 `assets::register_math` 同一个名字）。
fn math_path(formula: &Formula) -> String {
    crate::assets::math_name(&formula.typeset.svg)
}

/// 内容 → 名字的后半截（资源名和元素 id 共用）。
pub fn hash_of(bytes: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}.svg", hasher.finish())
}

/// 给所有画回复的 `TextView` 挂上的扩展：行内 + 块级公式。
///
/// 每帧重建 `MarkdownExtensions` 不会重排文档：`parser_revision` 固定，gpui-kit 只换渲染句柄。
pub fn extensions() -> MarkdownExtensions {
    MarkdownExtensions::default().plugin(InlineMath).plugin(BlockMath).parser_revision(1)
}
