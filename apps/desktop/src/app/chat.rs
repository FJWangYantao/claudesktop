//! 对话记录渲染：过程组（思考 / 工具 / 自动批准）、diff、权限卡、计划、正文、备注。
//!
//! 层级：回复正文最显眼；过程步骤一律一行灰色小字（左侧固定宽的图标格对齐），
//! 连续的几步收进可折叠的组（分组规则在 `Session::compute_layout`）。

use super::ChatApp;
use super::menu::{MenuChip, MenuList, diff_stat, menu_item, menu_popover};
use super::motion::{self, Reveal};
use super::rewind::file_name;
use super::widgets::Surface;
use crate::model::*;
use crate::session::{RowLayout, Session, StepGroup, WheelScroll};
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::{DiffView, PermissionOptionView};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::motion::{Presence, Transition};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::Position;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::borrow::Cow;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 同组相邻两步之间的行距
const STEP_GAP: Pixels = px(6.);
/// 块与块之间的行距
pub(super) const BLOCK_GAP: Pixels = px(16.);
/// 步骤行左侧的图标格宽度；组内细线对着它的中线
const ICON_COL: Pixels = px(16.);
/// 图标格 + 间距：步骤的展开内容（思考正文等）缩进到文字起点
const STEP_INDENT: Pixels = px(24.);
/// 输入框上方的计划最多露出几条
const PINNED_PLAN_ROWS: usize = 5;

/// 步骤行左侧的图标格：固定宽度居中，转圈 / 种类图标 / 失败叉都放这里，文字才对得齐。
fn icon_cell(icon: impl IntoElement) -> Div {
    div().w(ICON_COL).flex_shrink_0().flex().justify_center().items_center().child(icon)
}

/// 一行过程步骤的骨架：图标格 + 内容，13px 次要色，单行。
fn step_line(icon: impl IntoElement) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .text_size(TEXT_SMALL)
        .text_color(rgb(INK_SOFT))
        .child(icon_cell(icon))
}

/// 单行省略的文字段（悬停看全文）。
fn ellipsis(id: impl Into<ElementId>, text: SharedString) -> Stateful<Div> {
    let tip = text.clone();
    div()
        .id(id)
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .child(text)
}

/// 工具的状态图标：进行中转圈，失败红叉，完成显示种类图标。
/// 转圈换成结果图标时淡入、从小放大到原尺寸（`settle` 见 [`motion::settle`]）。
fn tool_status_icon(card: &ToolCard, settle: f32) -> AnyElement {
    let icon = match card.phase {
        ToolPhase::Running => return motion::spinner().into_any_element(),
        ToolPhase::Failed => Icon::new(IconAll::CircleX).size_3p5().text_color(rgb(RED)),
        ToolPhase::Done => {
            Icon::new(tool_icon(card.kind_key())).size_3p5().text_color(rgb(INK_SOFT))
        }
    };
    let scale = 0.6 + 0.4 * settle;
    div()
        .opacity(settle)
        .child(icon.transform(Transformation::scale(size(scale, scale))))
        .into_any_element()
}

/// 回复 / 思考共用的 Markdown 样式。
fn markdown_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.75))
        // 标题接口只收 Pixels，所以按当前缩放换算一次
        .heading_font_size(|level, _| {
            match level {
                1 => TEXT_HEADING,
                2 => TEXT_LEAD,
                _ => TEXT_LARGE,
            }
            .to_pixels(crate::theme::root_font_size())
        })
        .inline_code(HighlightStyle {
            background_color: Some(rgb(CODE_BG).into()),
            ..Default::default()
        })
        // 顶部多留一截给右上角的语言名 + 复制按钮，免得压住第一行代码（2rem = 32px）
        .code_block(StyleRefinement::default().bg(rgb(SAND)).pt(rems(2.)))
}

/// 代码块右上角的一组：语言名（有的话）+ 「复制」按钮。
/// 叠在代码块的沙色底上，按钮悬停用 `SELECTED`。
fn code_block_header(
    lang: Option<SharedString>,
    copied: Option<Instant>,
    on_copy: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &App,
) -> Div {
    let (icon, label) =
        if copied.is_some() { (IconAll::Check, "已复制") } else { (IconAll::Copy, "复制") };
    let pop = copied.and_then(|t| motion::enter(t.elapsed(), window, cx));
    div()
        .h_flex()
        .items_center()
        .gap_1()
        .bg(rgb(SAND))
        .text_size(TEXT_META)
        .text_color(rgb(INK_SOFT))
        .font_family(sans())
        .children(lang.filter(|l| !l.is_empty()).map(|l| div().px_1().child(l)))
        .child(
            ChatApp::quiet_button("copy-code", icon, Surface::Sand, cx)
                .xsmall()
                .px_1p5()
                .label(label)
                .when_some(pop, |b, (opacity, _)| b.opacity(opacity))
                .on_click(on_copy),
        )
}

/// 计划的一条：完成的打勾、变淡；进行中的橙点、加粗；待办空心圈。
/// 完成项不划线：删除线按拉丁字体的度量画，压在中文上偏低，像下划线。
fn plan_row(row: &PlanRow) -> Div {
    let (icon, color) = match row.status {
        PlanStatus::Done => (IconAll::CircleCheck, GREEN),
        PlanStatus::InProgress => (IconAll::CircleDot, CORAL),
        PlanStatus::Pending => (IconAll::Circle, INK_FAINT),
    };
    div()
        .h_flex()
        .items_start()
        .gap_2()
        // 图标对齐第一行文字的中线（14px 字 × 1.6 行高）
        .child(icon_cell(Icon::new(icon).size_3p5().text_color(rgb(color))).mt(px(4.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(TEXT_BODY)
                .line_height(LEADING)
                .map(|d| match row.status {
                    PlanStatus::Done => d.text_color(rgb(INK_SOFT)),
                    PlanStatus::InProgress => {
                        d.text_color(rgb(INK)).font_weight(FontWeight::MEDIUM)
                    }
                    PlanStatus::Pending => d.text_color(rgb(INK)),
                })
                .child(row.text.clone()),
        )
}

/// 计划卡片的标题行：图标 + 「计划」+ 进度。
fn plan_header(rows: &[PlanRow]) -> Div {
    let (done, total, _) = plan_progress(rows);
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .text_size(TEXT_SMALL)
        .child(icon_cell(Icon::new(IconAll::ListTodo).size_3p5()))
        .child("计划")
        .child(div().flex_1())
        .child(format!("{done}/{total}"))
}

impl ChatApp {
    /// 对话里的一行：可能先画过程组的组头，再画自己（组内步骤缩进、挂细线），最后留行距。
    /// `reveal` 是组内步骤的展开进度（不在组里的行恒为 1）：收起没走完时 `layout.body` 已经是
    /// false，但还要按进度裁切着画完。
    pub(super) fn render_row(
        &self,
        ix: usize,
        item: &ChatItem,
        layout: RowLayout,
        reveal: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut col = div().v_flex();
        if let Some(group) = layout.head {
            // 组头下面紧跟着组内步骤时只隔一小段
            let follows = layout.body || layout.tight;
            col =
                col.child(self.render_group_head(ix, group, cx).when(follows, |d| d.pb(STEP_GAP)));
        }
        if layout.body || reveal > 0. && layout.member {
            let body = self.render_item(ix, item, layout, window, cx);
            // 子 agent 的调用：在组内再缩进一层，挂在 Task 那一行下面
            let body = if matches!(item, ChatItem::Tool(card) if card.sub) {
                div().ml(px(7.)).pl(px(16.)).border_l_1().border_color(rgb(LINE)).child(body)
            } else {
                body
            };
            // 正在收起的步骤按展开时的样子画（缩进、挂细线、和下一步收紧）
            let (nested, tight) =
                if layout.body { (layout.nested, layout.tight) } else { (true, true) };
            let body = if nested {
                // 组内步骤：缩进到组头文字下面，左侧一条细线串起来；
                // 行距算在细线里面，相邻两行的线才连得上
                div()
                    .ml(px(7.))
                    .pl(px(16.))
                    .border_l_1()
                    .border_color(rgb(LINE))
                    .when(tight, |d| d.pb(STEP_GAP))
                    .child(body)
            } else {
                body
            };
            col = col.child(if reveal < 1. {
                Reveal::new(("step-body", ix), reveal, body).into_any_element()
            } else {
                body.into_any_element()
            });
        }
        // 正在收起、自己已不显示的步骤不留块间距（组头那行会留）
        let gap = Self::row_gap(layout);
        col.when(gap > px(0.), |d| d.pb(gap))
    }

    /// 这一行底下留多高的块间距：`content.rs` 里给高亮描边留位置也要用同一个值，
    /// 所以判断只放在这里（别再抄一份条件过去）。
    pub(super) fn row_gap(layout: RowLayout) -> Pixels {
        // 正在收起、自己已不显示的步骤不留块间距（组头那行会留）
        if !layout.tight && (layout.body || layout.head.is_some()) { BLOCK_GAP } else { px(0.) }
    }

    /// 过程组的组头：一行摘要（读了几个文件、跑了几条命令…），点开 / 收起；详细视图总是展开、不给开关。
    fn render_group_head(
        &self,
        ix: usize,
        group: StepGroup,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let s = self.session();
        let (summary, failed) =
            group_summary(s.transcript[ix..group.end].iter().filter(|i| i.has_content()));
        let toggle = self.settings.view_mode != ViewMode::Verbose;
        div()
            .id(("step-group", ix))
            .h_flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .when(toggle, |d| {
                d.cursor_pointer().hover(|d| d.text_color(rgb(INK))).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let active = this.active;
                        this.sessions[active].toggle_group(ix);
                        cx.notify();
                    },
                ))
            })
            .child(icon_cell(Icon::new(IconAll::ListChecks).size_3p5()))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(summary),
            )
            .when(failed > 0, |d| {
                d.child(
                    div().flex_shrink_0().text_color(rgb(RED)).child(format!("· {failed} 步失败")),
                )
            })
            .when(toggle, |d| {
                d.child(
                    Icon::new(if group.open {
                        IconAll::ChevronDown
                    } else {
                        IconAll::ChevronRight
                    })
                    .size_3(),
                )
            })
    }

    /// 思考：一行「已思考 12 秒 ›」，点开看正文（Markdown，次要色小一号字）；
    /// 流式时扫光「思考中…」；`verbose`（详细视图）时总是展开、不给开关。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_thinking(
        idx: usize,
        text: &str,
        streaming: bool,
        open: bool,
        elapsed_ms: Option<u64>,
        verbose: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let has_text = !text.trim().is_empty();
        let toggle = !verbose && has_text;
        let open = (open || verbose) && has_text;
        let header = step_line(Icon::new(IconAll::Brain).size_3p5().text_color(rgb(INK_SOFT)))
            .id(("think-toggle", idx))
            .when(toggle, |d| {
                d.cursor_pointer().hover(|d| d.text_color(rgb(INK))).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(ChatItem::Thinking { open, .. }) =
                            this.sessions[this.active].transcript.get_mut(idx)
                        {
                            *open = !*open;
                            cx.notify();
                        }
                    },
                ))
            })
            .child(if streaming {
                // 流式中高光扫过；减少动态效果时停在静态文字
                motion::shimmer_text("思考中…", INK_SOFT, INK).into_any_element()
            } else {
                // 用时是会变的数字：等宽，免得和别的思考块对不齐
                div()
                    .font_features(tabular_nums())
                    .child(thought_label(elapsed_ms))
                    .into_any_element()
            })
            .when(toggle, |d| {
                d.child(
                    Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                        .size_3(),
                )
            });

        let mut col = div().v_flex().child(header);
        // 展开 / 收起走弹簧；收起没走完之前正文照样画、按进度裁切。
        // 和标题之间的空隙算在正文里，收到底时不剩一截
        let progress = motion::reveal_progress(("think", idx), open, window, cx);
        if progress > 0. && has_text {
            let text = if streaming { close_dangling_markup(text) } else { Cow::Borrowed(text) };
            // 字号 / 颜色要设在 TextView 自己身上（它按主题取默认值，不继承外层）
            let body = div().pt_1().pl(STEP_INDENT).child(
                TextView::markdown(("think-md", idx), SharedString::from(text.trim().to_string()))
                    .style(markdown_style().paragraph_gap(rems(0.5)))
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .line_height(LEADING),
            );
            col = col.child(Reveal::new(("think-body", idx), progress, body));
        }
        col
    }

    /// 启动信息块（agent 在首条提示前推送的 Skills / Extensions 列表）；`verbose` 时总是展开。
    pub(super) fn render_info(
        idx: usize,
        text: &str,
        open: bool,
        verbose: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = open || verbose;
        let header = step_line(Icon::new(IconAll::Info).size_3p5().text_color(rgb(INK_SOFT)))
            .id(("info-toggle", idx))
            .when(!verbose, |d| {
                d.cursor_pointer().hover(|d| d.text_color(rgb(INK))).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(ChatItem::Info { open, .. }) =
                            this.sessions[this.active].transcript.get_mut(idx)
                        {
                            *open = !*open;
                            cx.notify();
                        }
                    },
                ))
            })
            .child("启动信息")
            .when(!verbose, |d| {
                d.child(
                    Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                        .size_3(),
                )
            });
        let mut col = div().v_flex().child(header);
        let progress = motion::reveal_progress(("info", idx), open, window, cx);
        if progress > 0. {
            let body = div()
                .pt_1()
                .pl(STEP_INDENT)
                .text_size(TEXT_SMALL)
                .font_family(MONO)
                .text_color(rgb(INK_SOFT))
                .line_height(LEADING)
                .child(text.trim().to_string());
            col = col.child(Reveal::new(("info-body", idx), progress, body));
        }
        col
    }

    /// 工具：一行「图标 名称 参数 · 结果」；带 diff 时 diff 卡片的表头兼当这一行。
    pub(super) fn render_tool(
        idx: usize,
        card: &ToolCard,
        verbose: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        if card.diffs.is_empty() {
            return Self::render_tool_plain(idx, card, verbose, window, cx);
        }
        // 文件改动：默认每份只显示前若干行，超出的部分展开 / 收起走弹簧
        let preview = (!verbose).then_some(DIFF_PREVIEW_ROWS);
        let rest = motion::reveal_progress(("diff", idx), card.expanded || verbose, window, cx);
        let settle = motion::settle(("tool", idx), card.phase != ToolPhase::Running, window, cx);
        let mut col = div().v_flex().gap_2();
        for (k, diff) in card.diffs.iter().enumerate() {
            let rest = (ElementId::from(("diff-rest", idx * 64 + k)), rest);
            col = col.child(Self::render_diff(diff, preview, Some(rest), Some((card, settle))));
        }
        // diff 已经说明了改了什么；结果摘要只在失败时有用（为什么没改成）
        if card.phase == ToolPhase::Failed && !card.result.is_empty() {
            col = col.child(
                div()
                    .pl(STEP_INDENT)
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(RED))
                    .child(card.result.clone()),
            );
        }
        let hidden: usize =
            card.diffs.iter().map(|d| d.lines.len().saturating_sub(DIFF_PREVIEW_ROWS)).sum();
        if !verbose && (hidden > 0 || card.expanded) {
            let label = if card.expanded {
                "收起".to_string()
            } else {
                format!("展开其余 {hidden} 行")
            };
            col = col.child(
                div()
                    .id(("diff-toggle", idx))
                    .pl_3()
                    .cursor_pointer()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .hover(|d| d.text_color(rgb(INK)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ChatItem::Tool(card)) =
                            this.sessions[this.active].transcript.get_mut(idx)
                        {
                            card.expanded = !card.expanded;
                            cx.notify();
                        }
                    }))
                    .child(label),
            );
        }
        col
    }

    /// 不带 diff 的工具：工具行；有输出时点开看预览（读到的内容 / 命令输出，
    /// 内核已截到前若干行）。`verbose`（详细视图）时总是展开、不给开关。
    fn render_tool_plain(
        idx: usize,
        card: &ToolCard,
        verbose: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let has_output = !card.output.trim().is_empty();
        let toggle = has_output && !verbose;
        let open = has_output && (card.expanded || verbose);
        let settle = motion::settle(("tool", idx), card.phase != ToolPhase::Running, window, cx);
        let line =
            Self::render_tool_line(idx, card, settle).id(("tool-toggle", idx)).when(toggle, |d| {
                d.cursor_pointer()
                    .hover(|d| d.text_color(rgb(INK)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ChatItem::Tool(card)) =
                            this.sessions[this.active].transcript.get_mut(idx)
                        {
                            card.expanded = !card.expanded;
                            cx.notify();
                        }
                    }))
                    .child(
                        div().flex_shrink_0().child(
                            Icon::new(if open {
                                IconAll::ChevronDown
                            } else {
                                IconAll::ChevronRight
                            })
                            .size_3(),
                        ),
                    )
            });
        let mut col = div().v_flex().child(line);
        let progress = motion::reveal_progress(("tool", idx), open, window, cx);
        if progress > 0. && has_output {
            let body = div().pt_1().pl(STEP_INDENT).child(
                div()
                    .v_flex()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .line_height(LEADING)
                    .text_color(rgb(INK_SOFT))
                    // 空行给个空格占住行高
                    .children(card.output.lines().map(|l| {
                        div().child(if l.is_empty() { " ".to_string() } else { l.to_string() })
                    })),
            );
            col = col.child(Reveal::new(("tool-body", idx), progress, body));
        }
        col
    }

    /// 不带 diff 的工具：一行，名称稍深、参数等宽、结果摘要接在后面（都单行省略，悬停看全文）。
    fn render_tool_line(idx: usize, card: &ToolCard, settle: f32) -> Div {
        let failed = card.phase == ToolPhase::Failed;
        step_line(tool_status_icon(card, settle))
            .child(
                div()
                    .flex_shrink_0()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK))
                    .child(card.name.clone()),
            )
            // 参数和结果按各自长度一起收缩（不给固定比例，免得截断后空出一截）
            .when(!card.arg.is_empty(), |d| {
                d.child(ellipsis(("tool-arg", idx), card.arg.clone()).font_family(MONO))
            })
            .when(!card.result.is_empty(), |d| {
                d.child(
                    ellipsis(("tool-result", idx), format!("· {}", card.result).into())
                        .when(failed, |d| d.text_color(rgb(RED))),
                )
            })
    }

    /// 自动批准的权限请求：作为过程中的一步，不打断。
    fn render_auto_approved(idx: usize, title: &SharedString, chosen: &SharedString) -> Div {
        step_line(Icon::new(IconAll::ShieldCheck).size_3p5().text_color(rgb(INK_SOFT)))
            .child(div().flex_shrink_0().child("已自动批准"))
            .child(ellipsis(("auto-title", idx), title.clone()).font_family(MONO))
            .child(div().flex_shrink_0().child(format!("· {chosen}")))
    }

    /// 权限请求：待处理时是醒目的卡片（标题说清动作和对象，按钮从常用到危险）；
    /// 应答后收成一行，不留大块残骸。
    /// 图片大图：gpui-kit 的对话框，宽约窗口八成，图按原比例缩进去（小图不放大）。
    fn open_image(path: PathBuf, window: &mut Window, cx: &mut App) {
        let viewport = window.viewport_size();
        let width = (viewport.width * 0.8).min(px(1200.));
        let height = viewport.height * 0.72;
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.w(width).margin_top(viewport.height * 0.06).child(
                div()
                    .w_full()
                    .h(height)
                    .flex()
                    .justify_center()
                    .items_center()
                    .child(img(path.clone()).size_full().object_fit(ObjectFit::ScaleDown)),
            )
        });
    }

    /// 上下文压缩：两侧细线、中间一行字，标出「从这里往前的对话换成了摘要」；完成后点一下展开摘要。
    fn render_compaction(
        idx: usize,
        state: &CompactState,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let done = matches!(state, CompactState::Done(_));
        let (indicator, label): (AnyElement, SharedString) = match state {
            CompactState::Running => {
                (motion::spinner().into_any_element(), "正在压缩上下文…".into())
            }
            CompactState::Done(_) => (
                Icon::new(IconAll::ListCollapse).size_3p5().into_any_element(),
                "上下文已压缩，较早的对话换成了摘要".into(),
            ),
            CompactState::Failed(message) => (
                Icon::new(IconAll::CircleAlert).size_3p5().text_color(rgb(RED)).into_any_element(),
                format!("压缩上下文失败：{message}").into(),
            ),
        };
        let rule = || div().flex_1().min_w(px(24.)).h(px(1.)).bg(rgb(LINE));
        let center = div()
            .id(("compaction", idx))
            .flex_shrink(1.)
            .min_w_0()
            .h_flex()
            .items_center()
            .gap_1p5()
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .child(indicator)
            .child(div().min_w_0().line_clamp(2).child(label))
            .when(done, |d| {
                d.cursor_pointer()
                    .hover(|d| d.text_color(rgb(INK)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ChatItem::Compaction { open, .. }) =
                            this.sessions[this.active].transcript.get_mut(idx)
                        {
                            *open = !*open;
                            cx.notify();
                        }
                    }))
                    .child(
                        Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                            .size_3(),
                    )
            });
        let mut col = div()
            .v_flex()
            .child(div().h_flex().items_center().gap_3().child(rule()).child(center).child(rule()));
        let progress = motion::reveal_progress(("compaction", idx), open && done, window, cx);
        if let (true, CompactState::Done(summary)) = (progress > 0., state) {
            let body = div().pt_2().child(
                div()
                    .px_4()
                    .py_3()
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .child(
                        TextView::markdown(("compaction-summary", idx), summary.clone())
                            .style(markdown_style())
                            .text_size(TEXT_BODY)
                            .text_color(rgb(INK))
                            .line_height(LEADING),
                    ),
            );
            col = col.child(Reveal::new(("compaction-body", idx), progress, body));
        }
        col
    }

    /// 失败卡片：发生了什么（原因原文）+ 可以怎么办（重试；超出上下文时先建议新开会话）。
    /// 重试 = 把这一轮的用户消息原样再发一遍；正在跑别的一轮时按钮变淡、点了没反应。
    fn render_failed(
        &self,
        idx: usize,
        message: &SharedString,
        kind: FailureKind,
        cx: &mut Context<Self>,
    ) -> Div {
        let s = self.session();
        let busy = s.busy;
        // 失败前已经有进展时给「继续」（接着做），「重试」会把做过的再做一遍
        // （判断在 Session 里：虚拟列表的测量指纹也要读它，见 `Session::row_key`）
        let resume = s.failure_resume(idx);
        // 已经翻篇的失败卡片只留说明和原文，不给按钮
        let latest = s.failure_is_latest(idx);
        // 主操作只占一个，按「先解决根因、再接着做」排：
        // 上下文溢出只能新开会话，Key 不对只能去改设置，其它情况有进展就继续、没进展就重试
        let primary = if kind == FailureKind::Overflow {
            "fail-new"
        } else if kind == FailureKind::Auth {
            "fail-settings"
        } else if resume {
            "fail-resume"
        } else {
            "fail-retry"
        };
        // 卡片自带边框，按钮不再描边：主操作赤陶实底，其余沙色实底（悬停 SELECTED）
        let button = |id: &'static str, icon: IconAll, label: &'static str| {
            Button::new((id, idx))
                .small()
                .px_3()
                .icon(icon)
                .label(label)
                .cursor_pointer()
                .disabled(busy)
                .with_variant(if id == primary {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Secondary
                })
        };
        div()
            .v_flex()
            .gap_2()
            .px_4()
            .py_3()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(icon_cell(Icon::new(IconAll::CircleAlert).size_4().text_color(rgb(RED))))
                    .child(
                        div()
                            .text_size(TEXT_BODY)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .child(kind.title()),
                    ),
            )
            .children(
                kind.hint().map(|hint| div().text_size(TEXT_BODY).text_color(rgb(INK)).child(hint)),
            )
            // 原文：网关 / 模型报的错，最多四行
            .child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .line_clamp(4)
                    .child(message.clone()),
            )
            .when(latest, |card| {
                card.child(
                    div()
                        .h_flex()
                        .gap_2()
                        .pt_1()
                        .when(kind == FailureKind::Overflow, |d| {
                            d.child(
                                button("fail-new", IconAll::MessageSquarePlus, "新开会话")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_session(cx);
                                        this.focus_input(window, cx);
                                    })),
                            )
                        })
                        .when(kind == FailureKind::Auth, |d| {
                            d.child(
                                button("fail-settings", IconAll::Settings, "打开设置").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.open_settings_at(1, window, cx)
                                    }),
                                ),
                            )
                        })
                        .when(resume, |d| {
                            d.child(
                                button("fail-resume", IconAll::Play, "继续")
                                    .on_click(cx.listener(|this, _, _, cx| this.resume_turn(cx))),
                            )
                        })
                        .child(
                            button("fail-retry", IconAll::RotateCcw, "重试").on_click(
                                cx.listener(move |this, _, _, cx| this.retry_turn(idx, cx)),
                            ),
                        ),
                )
            })
    }

    /// 权限卡：内核给的选项按钮之后是「拒绝并说明」。`keyed` = 这是最早一条待定的请求，
    /// 按钮上带 Alt+数字提示（见 keys.rs 的 `answer_key`）。
    pub(super) fn render_permission(
        idx: usize,
        title: &SharedString,
        options: &[PermissionOptionView],
        resolved: &Option<SharedString>,
        keyed: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        if let Some(label) = resolved {
            return step_line(Icon::new(IconAll::Shield).size_3p5().text_color(rgb(INK_SOFT)))
                .child(div().flex_shrink_0().child("权限请求"))
                .child(ellipsis(("perm-title", idx), title.clone()).font_family(MONO))
                .child(div().flex_shrink_0().child(format!("· {label}")));
        }

        // 按钮文字后面淡淡跟一个按键提示（不画 Kbd 底框：主按钮是赤陶实底）
        let key_hint = |n: usize| {
            super::keys::PERMISSION_KEYS.get(n).filter(|_| keyed).map(|key| {
                div().text_size(TEXT_META).opacity(0.7).child(Self::kbd(key).appearance(false))
            })
        };
        let mut buttons = div().h_flex().flex_wrap().gap_2();
        for (n, opt) in options.iter().enumerate() {
            let allow = opt.kind.starts_with("allow");
            let primary = opt.kind == "allow_once";
            let (id, name) = (opt.id.clone(), opt.name.clone());
            buttons = buttons.child(
                div()
                    .id(("perm", idx * 16 + n))
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    .px_3()
                    .py_1p5()
                    .rounded_lg()
                    .cursor_pointer()
                    .text_size(TEXT_BODY)
                    .when(primary, |d| {
                        d.bg(rgb(CORAL_DEEP))
                            .text_color(rgb(WHITE))
                            .hover(|d| d.bg(rgb(CORAL_DEEP_HOVER)))
                    })
                    // 卡片已经有一圈边框，里面的按钮不再描边（单层边框）：其余允许项用沙色实底；
                    // 拒绝只留红字、悬停才出底色（红字在沙色底上只有 4.0:1，白底上 4.7:1）
                    .when(!primary && allow, |d| {
                        d.bg(rgb(SAND)).hover(|d| d.bg(rgb(SELECTED))).text_color(rgb(INK))
                    })
                    .when(!allow, |d| d.hover(|d| d.bg(rgb(HOVER))).text_color(rgb(RED)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.resolve_permission(idx, Some((id.clone(), name.clone())), cx);
                    }))
                    .child(opt.name.clone())
                    .children(key_hint(n)),
            );
        }
        // 拒绝并说明：停下这一轮，请用户在输入框里说要怎么改（输入框里已经写了就直接发）
        buttons = buttons.child(
            div()
                .id(("perm-steer", idx))
                .h_flex()
                .items_center()
                .gap_1p5()
                .px_3()
                .py_1p5()
                .rounded_lg()
                .cursor_pointer()
                .text_size(TEXT_BODY)
                .text_color(rgb(INK_SOFT))
                .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.reject_and_steer(idx, window, cx);
                }))
                .child("拒绝并说明…")
                .children(key_hint(options.len())),
        );

        div()
            .v_flex()
            .gap_3()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(rgb(CORAL))
            .bg(rgb(CARD))
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(icon_cell(
                                Icon::new(IconAll::ShieldAlert).size_4().text_color(rgb(CORAL)),
                            ))
                            .child(
                                div()
                                    .text_size(TEXT_BODY)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("要允许这一步吗？"),
                            ),
                    )
                    .child(
                        div()
                            .pl(STEP_INDENT)
                            .text_size(TEXT_SMALL)
                            .font_family(MONO)
                            .text_color(rgb(INK_SOFT))
                            .line_height(LEADING)
                            .child(title.clone()),
                    ),
            )
            .child(div().pl(STEP_INDENT).child(buttons))
    }

    /// 回复末尾「改动 N 个文件 +a −b」：点开列出这一轮改过的文件，选一个跳到它的 diff。
    fn turn_files_menu(&self, idx: usize, files: Vec<TurnFile>, cx: &mut Context<Self>) -> Popover {
        let (added, removed) = files.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
        let label = match files.as_slice() {
            [one] => format!("改动 {}", file_name(&one.path)),
            many => format!("改动 {} 个文件", many.len()),
        };
        // Popover 的展开状态按 id 存在 window 上（`use_keyed_state`），不跟着外层
        // `div().id(("transcript", token))` 隔离：id 里带上会话 token，免得一个会话里
        // 开着的菜单在另一个会话同一行号上「继承」成打开状态
        let token = self.session().token;
        let id = SharedString::from(format!("turn-files-{token}-{idx}"));
        menu_popover(
            id.clone(),
            Anchor::BottomLeft,
            rems(24.),
            &cx.entity(),
            &self.menu_focus,
            MenuChip::new(IconAll::FileDiff, label).stat(added, removed),
            move |_, handle, popover| {
                let mut menu =
                    MenuList::new(SharedString::from(format!("{id}-list")), handle, popover)
                        .scroll(rems(22.5));
                for (i, f) in files.iter().enumerate() {
                    let item = f.item;
                    let name = file_name(&f.path);
                    let desc =
                        if f.created { format!("新文件 · {}", f.path) } else { f.path.clone() };
                    menu = menu.pick(
                        menu_item(("turn-file", i), IconAll::FileText, name, desc, false)
                            .child(diff_stat(f.added, f.removed)),
                        move |this, cx| this.reveal_change(item, cx),
                    );
                }
                menu
            },
        )
    }

    /// 从「改动 N 个文件」跳到某个文件的 diff：摘要视图里 diff 卡片是收起的。
    /// 只在内存里换回标准视图——这是「为看一眼」的临时切换，不能当成偏好写回设置文件
    /// （见 [`Self::set_view_mode`]）。
    fn reveal_change(&mut self, item: usize, cx: &mut Context<Self>) {
        if self.settings.view_mode == ViewMode::Summary {
            self.settings.view_mode = ViewMode::Normal;
            self.view_changed(cx);
        }
        self.reveal_message(self.active, item, cx);
    }

    /// 一个文件的 diff 卡片；`preview` = 默认露出的行数（None = 全部）。
    /// 超出的部分由 `rest`（元素 id, 展开进度）决定：进度 > 0 时按进度裁切着画出来。
    /// `tool`（工具, 结果图标的入场进度）给了时表头带上工具的状态图标和名称（兼当工具行）。
    pub(super) fn render_diff(
        diff: &DiffView,
        preview: Option<usize>,
        rest: Option<(ElementId, f32)>,
        tool: Option<(&ToolCard, f32)>,
    ) -> Div {
        let line_el = |line: &claudesktop_core::DiffLineView| {
            if line.tag == '~' {
                return div()
                    .h(px(18.))
                    .pl(px(40.))
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_FAINT))
                    .bg(rgb(SAND))
                    .child("⋯");
            }
            let (bg, fg) = match line.tag {
                '+' => (Some(DIFF_ADD_BG), DIFF_ADD_FG),
                '-' => (Some(DIFF_DEL_BG), DIFF_DEL_FG),
                _ => (None, INK_SOFT),
            };
            div()
                .h_flex()
                .items_start()
                .when_some(bg, |d, c| d.bg(rgb(c)))
                .child(
                    div()
                        .w(px(40.))
                        .flex_shrink_0()
                        .pr_2()
                        .flex()
                        .justify_end()
                        .text_color(rgb(INK_FAINT))
                        .child(format!("{}", line.no)),
                )
                .child(
                    div()
                        .w(px(14.))
                        .flex_shrink_0()
                        .text_color(rgb(fg))
                        .child(line.tag.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pr_3()
                        .text_color(rgb(if bg.is_some() { fg } else { INK }))
                        .child(line.text.clone()),
                )
        };
        let shown = preview.map_or(diff.lines.len(), |n| n.min(diff.lines.len()));
        let truncated = diff.truncated.then(|| {
            div()
                .px_3()
                .py_1()
                .font_family(sans())
                .text_color(rgb(INK_SOFT))
                .child("改动过大，其余部分未显示")
        });
        let mut body = div()
            .v_flex()
            .py_1()
            .font_family(MONO)
            .text_size(TEXT_SMALL)
            .line_height(LEADING)
            .children(diff.lines[..shown].iter().map(line_el));
        let hidden = &diff.lines[shown..];
        if hidden.is_empty() {
            body = body.children(truncated);
        } else if let Some((id, progress)) = rest.filter(|(_, p)| *p > 0.) {
            let more = div().v_flex().children(hidden.iter().map(line_el)).children(truncated);
            body = body.child(Reveal::new(id, progress, more));
        }
        let icon = match tool {
            Some((card, settle)) => tool_status_icon(card, settle),
            None => {
                Icon::new(IconAll::FileText).size_3p5().text_color(rgb(INK_SOFT)).into_any_element()
            }
        };
        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .bg(rgb(SAND))
            .border_b_1()
            .border_color(rgb(LINE))
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .child(icon_cell(icon))
            .children(tool.map(|(card, _)| {
                div()
                    .flex_shrink_0()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK))
                    .child(card.name.clone())
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(MONO)
                    .child(diff.path.clone()),
            )
            .when(diff.created, |d| d.child(div().flex_shrink_0().child("新文件")))
            .child(
                div()
                    .flex_shrink_0()
                    .h_flex()
                    .gap_1p5()
                    .font_family(MONO)
                    .child(div().text_color(rgb(DIFF_ADD_FG)).child(format!("+{}", diff.added)))
                    .child(div().text_color(rgb(DIFF_DEL_FG)).child(format!("−{}", diff.removed))),
            );
        div()
            .v_flex()
            .rounded_lg()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .overflow_hidden()
            .child(header)
            .child(body)
    }

    /// 对话里的计划：一轮进行中时它钉在输入框上方，这里只留一行进度（`compact`）；
    /// 结束后在这里完整显示最终状态。
    pub(super) fn render_plan(rows: &[PlanRow], compact: bool) -> Div {
        if compact {
            let (done, total, current) = plan_progress(rows);
            return step_line(Icon::new(IconAll::ListTodo).size_3p5().text_color(rgb(INK_SOFT)))
                .child(div().flex_shrink_0().child(format!("更新了计划 · {done}/{total}")))
                .children(current.map(|row| {
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(format!("· 正在：{}", row.text))
                }));
        }
        div()
            .v_flex()
            .gap_1()
            .px_3()
            .py_2p5()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .child(plan_header(rows).text_color(rgb(INK_SOFT)).pb_0p5())
            .children(rows.iter().map(plan_row))
    }

    /// 输入框上方钉着的计划（进行中的一轮才有）：最多露出 [`PINNED_PLAN_ROWS`] 条，
    /// 窗口跟着正在做的那一项走；点标题收成一行（只显示进度和当前项）。
    pub(super) fn render_pinned_plan(&self, cx: &mut Context<Self>) -> Option<Div> {
        let s = self.session();
        let ChatItem::Plan(rows) = &s.transcript[s.pinned_plan()?] else {
            return None;
        };
        if rows.is_empty() {
            return None;
        }
        let folded = s.plan_folded;
        let (_, total, current) = plan_progress(rows);
        let header = plan_header(rows)
            .id("pinned-plan")
            .text_color(rgb(INK_SOFT))
            .cursor_pointer()
            .hover(|d| d.text_color(rgb(INK)))
            .on_click(cx.listener(|this, _, _, cx| {
                let active = this.active;
                let s = &mut this.sessions[active];
                s.plan_folded = !s.plan_folded;
                cx.notify();
            }))
            .child(
                Icon::new(if folded { IconAll::ChevronUp } else { IconAll::ChevronDown }).size_3(),
            );
        let mut card = div()
            .v_flex()
            .gap_1()
            .px_3()
            .py_2()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .shadow_xs()
            .child(header);
        if folded {
            card = card.children(current.map(plan_row));
        } else {
            // 窗口：当前项前面留一条已完成的作参照
            let focus = rows.iter().position(|r| r.status != PlanStatus::Done).unwrap_or(total - 1);
            let start = focus.saturating_sub(1).min(total.saturating_sub(PINNED_PLAN_ROWS));
            let end = (start + PINNED_PLAN_ROWS).min(total);
            card = card.children(rows[start..end].iter().map(plan_row));
            let hidden = total - (end - start);
            if hidden > 0 {
                card = card.child(
                    div()
                        .pl(STEP_INDENT)
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .child(format!("另有 {hidden} 项")),
                );
            }
        }
        Some(card)
    }

    /// 切换视图模式并写回设置文件。
    pub(crate) fn set_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        if self.settings.view_mode == mode {
            return;
        }
        // 视图菜单 / 命令面板里两项都勾在内存里的那一项上，切完得让根视图重画
        // （列表那边由 sync_list 的测量指纹自己跟上）
        self.save_ui_pref(move |s| s.view_mode = mode, cx);
        self.view_changed(cx);
    }

    /// 视图模式换过之后：重算对话列表的分组 / 测量指纹（`Session::sync_list` 才看得到新模式）。
    pub(super) fn view_changed(&mut self, cx: &mut Context<Self>) {
        let view = self.settings.view_mode;
        let active = self.active;
        self.sessions[active].sync_list(view);
        cx.notify();
    }

    /// 往上翻过之后，对话区底部居中的「回到底部」圆钮；底下来了新内容时右上角带个赤陶点。
    /// 点一下跳回底部并恢复自动跟随。
    pub(super) fn render_jump_to_bottom(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let s = self.session();
        // 出现 / 消失淡入淡出、略微升起（每帧都要采样，状态才留得住）
        let presence = Presence::new("jump-to-bottom", s.scrolled_away())
            .transition(Transition::new(cx.theme().motion_tokens().duration_fast))
            .sample(window, cx);
        if !presence.should_render() {
            return None;
        }
        let shown = presence.progress;
        let unseen = s.has_unseen();
        let button = div()
            .id("jump-to-bottom")
            .relative()
            .size(px(32.))
            .rounded_full()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .shadow_sm()
            .flex()
            .justify_center()
            .items_center()
            .cursor_pointer()
            .hover(|d| d.bg(rgb(HOVER)))
            .tooltip(move |window, cx| {
                Tooltip::new(if unseen { "有新内容，回到底部" } else { "回到底部" })
                    .build(window, cx)
            })
            .opacity(shown)
            .top(px(6.) * (1. - shown))
            .on_click(cx.listener(|this, _, _, cx| this.start_jump_to_bottom(cx)))
            .child(Icon::new(IconAll::ArrowDown).size_4().text_color(rgb(INK_SOFT)))
            .when(unseen, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(-1.))
                        .right(px(-1.))
                        .size(px(9.))
                        .rounded_full()
                        .bg(rgb(CORAL))
                        .border_2()
                        .border_color(rgb(PAPER)),
                )
            });
        // 外层只占按钮那一条、不挂事件，不挡两侧的滚轮
        Some(div().absolute().bottom_3().left_0().right_0().flex().justify_center().child(button))
    }

    /// 点「回到底部」：离得远（超过一屏半）先跳到离底一屏的地方，剩下的一屏滑下去，
    /// 到底后恢复自动跟随。减少动态效果时直接到底。
    pub(super) fn start_jump_to_bottom(&mut self, cx: &mut Context<Self>) {
        let s = &mut self.sessions[self.active];
        s.wheel = None;
        let remaining = distance_to_end(&s.list);
        if cx.reduce_motion() || remaining <= px(1.) {
            s.follow_tail();
            cx.notify();
            return;
        }
        let viewport = s.list.viewport_bounds().size.height;
        if remaining > viewport * 1.5 {
            s.list.scroll_by(remaining - viewport);
        }
        let now = Instant::now();
        self.jump = Some(motion::JumpAnim { token: s.token, started: now, last: now });
        cx.notify();
    }

    /// 平滑回到底部的一帧（`render` 开头调）：剩下的距离按指数衰减地滑过去，
    /// 够近了或走了太久就交给自动跟随。切走会话、用户自己滚到底了就停。
    pub(super) fn step_jump_to_bottom(&mut self, window: &mut Window) {
        let Some(mut anim) = self.jump.take() else {
            return;
        };
        let s = &mut self.sessions[self.active];
        if anim.token != s.token || s.list.is_following_tail() {
            return;
        }
        let remaining = distance_to_end(&s.list);
        if remaining <= px(1.) || anim.started.elapsed() > motion::JUMP_MAX {
            s.follow_tail();
            return;
        }
        let now = Instant::now();
        let dt = (now - anim.last).as_secs_f32();
        anim.last = now;
        let k = 1. - (-dt / motion::JUMP_TAU).exp();
        s.list.scroll_by((remaining * k).max(px(1.)).min(remaining));
        self.jump = Some(anim);
        window.request_animation_frame();
    }

    /// 滚轮往末尾滚了 `delta`（负 = 往开头）：只推进目标，由 [`Self::step_wheel`] 每帧滑过去，
    /// 一格不再硬跳 60px。触控板的精确位移本来就连续，和减少动态效果时一样直接滚到位。
    /// 只通知内容区：滚动不影响输入区。
    pub(super) fn wheel_by(&mut self, delta: Pixels, precise: bool, cx: &mut Context<Self>) {
        self.jump = None;
        let s = &mut self.sessions[self.active];
        let top = scroll_top(&s.list);
        let max = s.list.max_offset_for_scrollbar().y.max(px(0.));
        if precise || cx.reduce_motion() {
            s.wheel = None;
            s.list.scroll_by((top + delta).clamp(px(0.), max) - top);
        } else {
            // 接着上一格没滑完的目标往前推；反过来滚时从当前位置算，不先把剩下那段走完
            let base = match s.wheel {
                Some(w) if (w.target > top) == (delta > px(0.)) => w.target,
                _ => top,
            };
            let now = Instant::now();
            // 新的一串从「上一帧」算起，第一帧就走一帧的量，不是从零起步
            let last = s.wheel.map_or(now.checked_sub(FRAME).unwrap_or(now), |w| w.last);
            s.wheel =
                Some(WheelScroll { target: (base + delta).clamp(px(0.), max), started: now, last });
        }
        self.content_view.update(cx, |_, cx| cx.notify());
    }

    /// 滚轮平滑的一帧（`render_content` 开头调）：剩下的距离按指数衰减地滑过去。
    pub(super) fn step_wheel(&mut self, window: &mut Window) {
        let s = &mut self.sessions[self.active];
        let Some(mut w) = s.wheel.take() else {
            return;
        };
        if w.started.elapsed() > motion::WHEEL_MAX {
            return;
        }
        let now = Instant::now();
        let dt = (now - w.last).as_secs_f32().min(0.1);
        w.last = now;
        // 期间有行重测过，可滚的范围会变：目标跟着收进去，免得够不着一直排帧
        w.target = w.target.min(s.list.max_offset_for_scrollbar().y.max(px(0.)));
        let rest = w.target - scroll_top(&s.list);
        let step = rest * (1. - (-dt / motion::WHEEL_TAU).exp());
        if (rest - step).abs() < px(0.5) {
            s.list.scroll_by(rest);
            return;
        }
        s.list.scroll_by(step);
        s.wheel = Some(w);
        window.request_animation_frame();
    }

    /// 鼠标进出对话里的一条消息（下方操作按钮的淡入淡出跟着它）。
    fn set_hovered_msg(&mut self, idx: usize, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered {
            Some(idx)
        } else if self.hovered_msg == Some(idx) {
            None
        } else {
            self.hovered_msg
        };
        if next != self.hovered_msg {
            self.hovered_msg = next;
            cx.notify();
        }
    }

    /// 「复制」按钮：点了把 `text(当前会话, idx)` 放进剪贴板，图标短暂变成对勾、文字变「已复制」。
    fn copy_action(
        &self,
        id: impl Into<ElementId>,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
        text: impl Fn(&Session, usize) -> String + 'static,
    ) -> Button {
        let copied = self.copied == Some((self.session().token, idx, None));
        let (icon, label) =
            if copied { (IconAll::Check, "已复制") } else { (IconAll::Copy, "复制") };
        // 变成对勾时淡入一下，确认点上了
        let pop =
            self.copied_at.filter(|_| copied).and_then(|t| motion::enter(t.elapsed(), window, cx));
        Self::message_action(id, icon, label, cx)
            .when_some(pop, |b, (opacity, _)| b.opacity(opacity))
            .on_click(cx.listener(move |this, _, _, cx| {
                let content = text(this.session(), idx);
                this.copy_to_clipboard(idx, None, content, cx);
            }))
    }

    /// 写剪贴板并记下「刚复制的是哪条（哪个代码块）」，1.5 秒后自动清掉对勾。
    pub(crate) fn copy_to_clipboard(
        &mut self,
        idx: usize,
        block: Option<usize>,
        text: String,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        let key = (self.session().token, idx, block);
        self.copied = Some(key);
        self.copied_at = Some(Instant::now());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |app, cx| {
                if app.copied == Some(key) {
                    app.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 「编辑」：把这条用户消息放回输入框（输入框里已有草稿就接在后面，不覆盖），光标放到末尾。
    pub(crate) fn edit_message(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ChatItem::User { text, .. }) = self.session().transcript.get(idx) else {
            return;
        };
        let text = text.to_string();
        self.input.update(cx, |state, cx| {
            let draft = state.value().to_string();
            let next = if draft.trim().is_empty() { text } else { format!("{draft}\n{text}") };
            let last_line = next.matches('\n').count() as u32;
            state.set_value(next, window, cx);
            // 列号超出会被截到行尾
            state.set_cursor_position(Position::new(last_line, u32::MAX), window, cx);
        });
        cx.notify();
    }

    /// 用户消息气泡；悬停时下方淡入「复制 / 编辑 / 回退 / 在另一个内核上重跑」。刚发出的一条从下方淡入升起。
    fn render_user(
        &self,
        idx: usize,
        text: &SharedString,
        images: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        // 回退浮层开着时操作行常显（鼠标移到浮层上就离开了这条消息）
        let rewinding = self.rewind.as_ref().is_some_and(|p| p.item() == idx);
        let fade = motion::hover_opacity(
            ("user-actions", idx),
            self.hovered_msg == Some(idx) || rewinding,
            window,
            cx,
        );
        let current = self.session().kernel;
        let others = KernelChoice::ALL.into_iter().filter(|k| {
            *k != current
                && *k != KernelChoice::Demo
                && (*k != KernelChoice::Builtin || self.settings.builtin.is_configured())
        });
        let edit_tip = if images.is_empty() {
            "放回输入框，改完重新发送"
        } else {
            "放回输入框（图片要重新附上），改完重新发送"
        };
        let mut actions = div()
            .h_flex()
            .justify_end()
            .gap_1()
            .opacity(fade)
            .child(self.copy_action(("copy-user", idx), idx, window, cx, |s, idx| {
                match &s.transcript[idx] {
                    ChatItem::User { text, .. } => text.to_string(),
                    _ => String::new(),
                }
            }))
            .child(
                Self::message_action(("edit-user", idx), IconAll::Pencil, "编辑", cx)
                    .tooltip(edit_tip)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.edit_message(idx, window, cx)),
                    ),
            )
            .child(self.rewind_action(idx, cx));
        for kind in others {
            actions = actions.child(
                Self::message_action(
                    ("rerun", idx * 4 + kind.index()),
                    IconAll::RefreshCw,
                    format!("在「{}」上重跑", kind.label()),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.rerun_on(idx, kind, cx))),
            );
        }
        // 气泡：有文字才画文本气泡，图片另起一行缩略图（丢了文件就跳过）。
        let bubble = if text.is_empty() {
            None
        } else {
            Some(
                div()
                    .max_w(relative(0.85))
                    .bg(rgb(SAND))
                    .text_color(rgb(INK))
                    .rounded_2xl()
                    .px_4()
                    .py_2p5()
                    .text_size(TEXT_LARGE)
                    .line_height(LEADING)
                    .child(text.clone()),
            )
        };
        let thumbs = if images.is_empty() {
            None
        } else {
            Some(div().h_flex().justify_end().gap_1p5().flex_wrap().max_w(relative(0.85)).children(
                images.iter().filter(|p| p.exists()).enumerate().map(|(n, p)| {
                    let path = p.clone();
                    // 点缩略图看大图（Esc / 点遮罩 / 右上角 × 关闭）
                    div()
                        .id(("user-image", idx * 16 + n))
                        .cursor_pointer()
                        .rounded_md()
                        .hover(|d| d.opacity(0.85))
                        .on_click(cx.listener(move |_, _, window, cx| {
                            Self::open_image(path.clone(), window, cx)
                        }))
                        .child(img(p.clone()).size_20().rounded_md().object_fit(ObjectFit::Cover))
                }),
            ))
        };
        let block = div()
            .id(("user-msg", idx))
            .v_flex()
            .items_end()
            .gap_1()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_hovered_msg(idx, *hovered, cx)
            }))
            .children(thumbs)
            .children(bubble)
            .child(actions);
        motion::entering(block, self.session().fresh_user(idx), window, cx)
    }

    pub(super) fn render_item(
        &self,
        idx: usize,
        item: &ChatItem,
        layout: RowLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let verbose = self.settings.view_mode == ViewMode::Verbose;
        match item {
            ChatItem::User { text, images } => self.render_user(idx, text, images, window, cx),
            ChatItem::Thinking { text, streaming, open, elapsed_ms, .. } => Self::render_thinking(
                idx,
                text,
                *streaming,
                *open,
                *elapsed_ms,
                verbose,
                window,
                cx,
            ),
            ChatItem::Info { text, open } => {
                Self::render_info(idx, text, *open, verbose, window, cx)
            }
            ChatItem::Tool(card) => Self::render_tool(idx, card, verbose, window, cx),
            ChatItem::AutoApproved { title, chosen } => {
                Self::render_auto_approved(idx, title, chosen)
            }
            ChatItem::Permission { title, options, resolved, .. } => {
                let keyed = self.session().pending_permission() == Some(idx);
                Self::render_permission(idx, title, options, resolved, keyed, cx)
            }
            ChatItem::Plan(rows) => Self::render_plan(rows, layout.compact),
            ChatItem::Agent(text) => {
                let s = self.session();
                let streaming = s.busy && idx + 1 == s.transcript.len();
                let text =
                    if streaming { close_dangling_markup(text).into_owned() } else { text.clone() };
                // 代码块右上角：语言名 + 「复制」（点完短暂变对勾，和消息下方的复制同一套状态）
                let token = s.token;
                let copied_block = match self.copied {
                    Some((t, i, block)) if t == token && i == idx => block,
                    _ => None,
                };
                let copied_at = self.copied_at;
                let app = cx.entity().downgrade();
                let body = div().min_w_0().text_size(TEXT_LARGE).line_height(LEADING).child(
                    TextView::markdown(("agent", idx), SharedString::from(text))
                        .style(markdown_style())
                        .stream_fade(true)
                        .code_block_actions(move |block, window, cx| {
                            let start = block.span.map(|span| span.start).unwrap_or(0);
                            let code = block.code();
                            let app = app.clone();
                            code_block_header(
                                block.lang(),
                                copied_at.filter(|_| copied_block == Some(start)),
                                move |_, _, cx| {
                                    let _ = app.update(cx, |this, cx| {
                                        this.copy_to_clipboard(
                                            idx,
                                            Some(start),
                                            code.to_string(),
                                            cx,
                                        )
                                    });
                                },
                                window,
                                cx,
                            )
                        }),
                );
                if !s.is_turn_end(idx) {
                    return body;
                }
                // 一轮回复的末尾挂「复制」和这一轮的用时 / 用量：最后一轮常显，
                // 更早的悬停才淡入（只改透明度，不挤布局）
                let latest =
                    !s.transcript[idx + 1..].iter().any(|i| matches!(i, ChatItem::User { .. }));
                let hovered = self.hovered_msg == Some(idx);
                let fade = (!latest)
                    .then(|| motion::hover_opacity(("agent-actions", idx), hovered, window, cx));
                let meta = s.turn_summary(idx).and_then(super::status::turn_meta);
                let files = s.turn_files(idx);
                let actions = div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .pt_2()
                    .child(self.copy_action(("copy-agent", idx), idx, window, cx, |s, idx| {
                        s.turn_text(idx)
                    }))
                    .children(meta.map(|m| {
                        // 和运行中的状态行同一套数字字形，一轮结束换过来时不变样
                        div()
                            .text_size(TEXT_META)
                            .text_color(rgb(INK_SOFT))
                            .font_features(tabular_nums())
                            .child(m)
                    }))
                    .when(!files.is_empty(), |d| d.child(self.turn_files_menu(idx, files, cx)))
                    .when_some(fade, |d, opacity| d.opacity(opacity));
                div().child(
                    div()
                        .id(("agent-msg", idx))
                        .v_flex()
                        .min_w_0()
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            this.set_hovered_msg(idx, *hovered, cx)
                        }))
                        .child(body)
                        .child(actions),
                )
            }
            ChatItem::Failed { message, kind } => self.render_failed(idx, message, *kind, cx),
            ChatItem::Compaction { state, open } => {
                Self::render_compaction(idx, state, *open, window, cx)
            }
            // 一轮层面的提示（续接、停止…）：和正文左对齐，图标打头
            ChatItem::Note(text) => {
                step_line(Icon::new(IconAll::Info).size_3p5().text_color(rgb(INK_SOFT)))
                    .child(div().flex_1().min_w_0().child(text.clone()))
            }
        }
    }
}

/// 列表离底部还有多远（按已量过的行高估）。
/// 一帧的时长（滚轮平滑起步时用）
const FRAME: Duration = Duration::from_millis(16);

/// 列表当前滚到离顶部多远（像素）。
fn scroll_top(list: &ListState) -> Pixels {
    -list.scroll_px_offset_for_scrollbar().y
}

fn distance_to_end(list: &ListState) -> Pixels {
    (list.max_offset_for_scrollbar().y + list.scroll_px_offset_for_scrollbar().y).max(px(0.))
}

/// 流式输出停在半截标记上时（`` `foo `` 或没收尾的 ``` 围栏、`**粗体`），渲染前临时补齐，
/// 免得标记原样露出来、等收尾后整段又跳成代码 / 粗体样式。
fn close_dangling_markup(text: &str) -> Cow<'_, str> {
    let mut in_fence = false;
    // 当前段落（围栏外）里的反引号个数；行内代码不跨段落
    let mut ticks = 0;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            ticks = 0;
        } else if !in_fence {
            if line.trim().is_empty() {
                ticks = 0;
            } else {
                ticks += line.matches('`').count();
            }
        }
    }
    if in_fence {
        Cow::Owned(format!("{text}\n```"))
    } else if ticks % 2 == 1 {
        Cow::Owned(format!("{text}`"))
    } else {
        close_dangling_bold(text)
    }
}

/// 最后一行里没收尾的 `**`：后面已经有字就补上收尾（先去掉末尾空白，否则不算收尾），
/// 后面还没字（`- **`）就先不显示这个标记。只看最后一行，行内代码里的星号不算。
fn close_dangling_bold(text: &str) -> Cow<'_, str> {
    let start = text.rfind('\n').map_or(0, |i| i + 1);
    // 按反引号切开，偶数段在行内代码外
    let (mut count, mut open, mut at) = (0, None, start);
    for (i, part) in text[start..].split('`').enumerate() {
        if i % 2 == 0 {
            for (j, _) in part.match_indices("**") {
                count += 1;
                open = Some(at + j);
            }
        }
        at += part.len() + 1;
    }
    match open {
        Some(open) if count % 2 == 1 && text[open + 2..].trim().is_empty() => {
            Cow::Borrowed(&text[..open])
        }
        Some(_) if count % 2 == 1 => Cow::Owned(format!("{}**", text.trim_end())),
        _ => Cow::Borrowed(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn dangling_code_is_closed_while_streaming() {
        assert_eq!(close_dangling_markup("事件来自 ACP 的 `sess"), "事件来自 ACP 的 `sess`");
        assert_eq!(close_dangling_markup("用 `a` 和 `b`"), "用 `a` 和 `b`");
        assert_eq!(close_dangling_markup("```rust\nfn a() {"), "```rust\nfn a() {\n```");
        // 上一段的单个反引号不影响下一段
        assert_eq!(close_dangling_markup("a ` b\n\nc"), "a ` b\n\nc");
        assert_eq!(close_dangling_markup("```\n`\n```\nok"), "```\n`\n```\nok");
    }

    #[core::prelude::v1::test]
    fn dangling_bold_is_closed_or_hidden_while_streaming() {
        assert_eq!(close_dangling_markup("- **思考"), "- **思考**");
        assert_eq!(close_dangling_markup("- **思考 "), "- **思考**");
        // 标记后面还没字：先藏起来
        assert_eq!(close_dangling_markup("说明：\n- **"), "说明：\n- ");
        assert_eq!(close_dangling_markup("**a** 和 **b"), "**a** 和 **b**");
        assert_eq!(close_dangling_markup("**a** 和 **b**"), "**a** 和 **b**");
        // 行内代码里的星号、上一行的星号不算
        assert_eq!(close_dangling_markup("用 `**` 表示粗体"), "用 `**` 表示粗体");
        assert_eq!(close_dangling_markup("**a\nb"), "**a\nb");
    }
}
