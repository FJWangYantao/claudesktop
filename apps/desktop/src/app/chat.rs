//! 对话记录渲染：过程组（思考 / 工具 / 自动批准）、diff、权限卡、计划、正文、备注。
//!
//! 层级：回复正文最显眼；过程步骤一律一行灰色小字（左侧固定宽的图标格对齐），
//! 连续的几步收进可折叠的组（分组规则在 `Session::compute_layout`）。

use super::ChatApp;
use crate::model::*;
use crate::session::{RowLayout, Session, StepGroup};
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::{DiffView, PermissionOptionView};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::Position;
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::borrow::Cow;
use std::path::PathBuf;
use std::time::Duration;

/// 同组相邻两步之间的行距
const STEP_GAP: Pixels = px(6.);
/// 块与块之间的行距
const BLOCK_GAP: Pixels = px(16.);
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
fn tool_status_icon(card: &ToolCard) -> AnyElement {
    match card.phase {
        ToolPhase::Running => Spinner::new().xsmall().color(rgb(CORAL).into()).into_any_element(),
        ToolPhase::Failed => {
            Icon::new(IconAll::CircleX).size_3p5().text_color(rgb(RED)).into_any_element()
        }
        ToolPhase::Done => Icon::new(tool_icon(card.kind_key()))
            .size_3p5()
            .text_color(rgb(INK_SOFT))
            .into_any_element(),
    }
}

/// 回复 / 思考共用的 Markdown 样式。
fn markdown_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.75))
        .heading_font_size(|level, _| match level {
            1 => TEXT_HEADING,
            2 => TEXT_LEAD,
            _ => TEXT_LARGE,
        })
        .inline_code(HighlightStyle {
            background_color: Some(rgb(CODE_BG).into()),
            ..Default::default()
        })
        // 顶部多留一截给右上角的语言名 + 复制按钮，免得压住第一行代码
        .code_block(StyleRefinement::default().bg(rgb(SAND)).pt(px(32.)))
}

/// 代码块右上角的一组：语言名（有的话）+ 「复制」按钮。
/// 叠在代码块的沙色底上，按钮悬停用 `SELECTED`。
fn code_block_header(
    lang: Option<SharedString>,
    copied: bool,
    on_copy: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let (icon, label) =
        if copied { (IconAll::Check, "已复制") } else { (IconAll::Copy, "复制") };
    div()
        .h_flex()
        .items_center()
        .gap_1()
        .bg(rgb(SAND))
        .text_size(TEXT_META)
        .text_color(rgb(INK_SOFT))
        .font_family(SANS)
        .children(lang.filter(|l| !l.is_empty()).map(|l| div().px_1().child(l)))
        .child(
            div()
                .id("copy-code")
                .h_flex()
                .items_center()
                .gap_1()
                .px_1p5()
                .py_0p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(SELECTED)))
                .child(Icon::new(icon).size_3())
                .child(label)
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
    pub(super) fn render_row(
        &self,
        ix: usize,
        item: &ChatItem,
        layout: RowLayout,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut col = div().v_flex();
        if let Some(group) = layout.head {
            // 组头下面紧跟着组内步骤时只隔一小段
            let follows = layout.body || layout.tight;
            col =
                col.child(self.render_group_head(ix, group, cx).when(follows, |d| d.pb(STEP_GAP)));
        }
        if layout.body {
            let body = self.render_item(ix, item, layout, cx);
            col = col.child(if layout.nested {
                // 组内步骤：缩进到组头文字下面，左侧一条细线串起来；
                // 行距算在细线里面，相邻两行的线才连得上
                div()
                    .ml(px(7.))
                    .pl(px(16.))
                    .border_l_1()
                    .border_color(rgb(LINE))
                    .when(layout.tight, |d| d.pb(STEP_GAP))
                    .child(body)
            } else {
                body
            });
        }
        col.when(!layout.tight, |d| d.pb(BLOCK_GAP))
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
                // 流式中高光扫过；系统开了「减少动画」时组件自己退回静态文字
                ShimmerText::new("思考中…")
                    .id(("think-shimmer", idx))
                    .highlight_color(rgb(INK))
                    .into_any_element()
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

        let mut col = div().v_flex().gap_1().child(header);
        if open {
            let text = if streaming { close_dangling_code(text) } else { Cow::Borrowed(text) };
            // 字号 / 颜色要设在 TextView 自己身上（它按主题取默认值，不继承外层）
            col = col.child(
                div().pl(STEP_INDENT).child(
                    TextView::markdown(
                        ("think-md", idx),
                        SharedString::from(text.trim().to_string()),
                    )
                    .style(markdown_style().paragraph_gap(rems(0.5)))
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .line_height(LEADING),
                ),
            );
        }
        col
    }

    /// 启动信息块（agent 在首条提示前推送的 Skills / Extensions 列表）；`verbose` 时总是展开。
    pub(super) fn render_info(
        idx: usize,
        text: &str,
        open: bool,
        verbose: bool,
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
        let mut col = div().v_flex().gap_1().child(header);
        if open {
            col = col.child(
                div()
                    .pl(STEP_INDENT)
                    .text_size(TEXT_SMALL)
                    .font_family(MONO)
                    .text_color(rgb(INK_SOFT))
                    .line_height(LEADING)
                    .child(text.trim().to_string()),
            );
        }
        col
    }

    /// 工具：一行「图标 名称 参数 · 结果」；带 diff 时 diff 卡片的表头兼当这一行。
    pub(super) fn render_tool(
        idx: usize,
        card: &ToolCard,
        verbose: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        if card.diffs.is_empty() {
            return Self::render_tool_plain(idx, card, verbose, cx);
        }
        // 文件改动：默认每份只显示前若干行，超出给一个展开开关
        let limit = (!card.expanded && !verbose).then_some(DIFF_PREVIEW_ROWS);
        let mut col = div().v_flex().gap_2();
        for diff in &card.diffs {
            col = col.child(Self::render_diff(diff, limit, Some(card)));
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
        cx: &mut Context<Self>,
    ) -> Div {
        let has_output = !card.output.trim().is_empty();
        let toggle = has_output && !verbose;
        let open = has_output && (card.expanded || verbose);
        let line = Self::render_tool_line(idx, card).id(("tool-toggle", idx)).when(toggle, |d| {
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
                        Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                            .size_3(),
                    ),
                )
        });
        let mut col = div().v_flex().gap_1().child(line);
        if open {
            col = col.child(
                div().pl(STEP_INDENT).child(
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
                ),
            );
        }
        col
    }

    /// 不带 diff 的工具：一行，名称稍深、参数等宽、结果摘要接在后面（都单行省略，悬停看全文）。
    fn render_tool_line(idx: usize, card: &ToolCard) -> Div {
        let failed = card.phase == ToolPhase::Failed;
        step_line(tool_status_icon(card))
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
        cx: &mut Context<Self>,
    ) -> Div {
        let done = matches!(state, CompactState::Done(_));
        let (indicator, label): (AnyElement, SharedString) = match state {
            CompactState::Running => (
                Spinner::new().xsmall().color(rgb(CORAL).into()).into_any_element(),
                "正在压缩上下文…".into(),
            ),
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
            .gap_2()
            .child(div().h_flex().items_center().gap_3().child(rule()).child(center).child(rule()));
        if let (true, CompactState::Done(summary)) = (open, state) {
            col = col.child(
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
        }
        col
    }

    /// 失败卡片：发生了什么（原因原文）+ 可以怎么办（重试；超出上下文时先建议新开会话）。
    /// 重试 = 把这一轮的用户消息原样再发一遍；正在跑别的一轮时按钮变淡、点了没反应。
    fn render_failed(
        &self,
        idx: usize,
        message: &SharedString,
        overflow: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let busy = self.session().busy;
        let title = if overflow { "超出了模型的上下文长度" } else { "本轮失败" };
        let button = |id: &'static str, icon: IconAll, label: &'static str, primary: bool| {
            div()
                .id((id, idx))
                .h_flex()
                .items_center()
                .gap_1p5()
                .px_3()
                .py_1()
                .rounded_lg()
                .text_size(TEXT_BODY)
                .map(|d| match (primary, busy) {
                    (_, true) => d.bg(rgb(SAND)).text_color(rgb(INK_FAINT)),
                    (true, false) => d
                        .bg(rgb(CORAL_DEEP))
                        .text_color(rgb(WHITE))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(CORAL_DEEP_HOVER))),
                    (false, false) => d
                        .bg(rgb(SAND))
                        .text_color(rgb(INK))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(SELECTED))),
                })
                .child(Icon::new(icon).size_3p5())
                .child(label)
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
                            .child(title),
                    ),
            )
            .when(overflow, |d| {
                d.child(
                    div()
                        .text_size(TEXT_BODY)
                        .text_color(rgb(INK))
                        .child("这个会话已经长到模型装不下了。新开一个会话接着做，或者换一个上下文更大的模型再重试。"),
                )
            })
            // 原文：网关 / 模型报的错，最多四行
            .child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .line_clamp(4)
                    .child(message.clone()),
            )
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .pt_1()
                    .when(overflow, |d| {
                        d.child(button("fail-new", IconAll::MessageSquarePlus, "新开会话", true).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.add_session(cx);
                                this.focus_input(window, cx);
                            }),
                        ))
                    })
                    .child(button("fail-retry", IconAll::RotateCcw, "重试", false).on_click(
                        cx.listener(move |this, _, _, cx| this.retry_turn(idx, cx)),
                    )),
            )
    }

    pub(super) fn render_permission(
        idx: usize,
        title: &SharedString,
        options: &[PermissionOptionView],
        resolved: &Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> Div {
        if let Some(label) = resolved {
            return step_line(Icon::new(IconAll::Shield).size_3p5().text_color(rgb(INK_SOFT)))
                .child(div().flex_shrink_0().child("权限请求"))
                .child(ellipsis(("perm-title", idx), title.clone()).font_family(MONO))
                .child(div().flex_shrink_0().child(format!("· {label}")));
        }

        let mut buttons = div().h_flex().flex_wrap().gap_2();
        for (n, opt) in options.iter().enumerate() {
            let allow = opt.kind.starts_with("allow");
            let primary = opt.kind == "allow_once";
            let (id, name) = (opt.id.clone(), opt.name.clone());
            buttons = buttons.child(
                div()
                    .id(("perm", idx * 16 + n))
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
                    .child(opt.name.clone()),
            );
        }

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

    /// 一个文件的 diff 卡片；`limit` = 最多显示的行数（None = 全部）。
    /// `tool` 给了时表头带上工具的状态图标和名称（兼当工具行）。
    pub(super) fn render_diff(
        diff: &DiffView,
        limit: Option<usize>,
        tool: Option<&ToolCard>,
    ) -> Div {
        let shown = limit.map_or(diff.lines.len(), |n| n.min(diff.lines.len()));
        let mut body =
            div().v_flex().py_1().font_family(MONO).text_size(TEXT_SMALL).line_height(LEADING);
        for line in &diff.lines[..shown] {
            if line.tag == '~' {
                body = body.child(
                    div()
                        .h(px(18.))
                        .pl(px(40.))
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_FAINT))
                        .bg(rgb(SAND))
                        .child("⋯"),
                );
                continue;
            }
            let (bg, fg) = match line.tag {
                '+' => (Some(DIFF_ADD_BG), DIFF_ADD_FG),
                '-' => (Some(DIFF_DEL_BG), DIFF_DEL_FG),
                _ => (None, INK_SOFT),
            };
            body = body.child(
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
                    ),
            );
        }
        if diff.truncated && shown == diff.lines.len() {
            body = body.child(
                div()
                    .px_3()
                    .py_1()
                    .font_family(SANS)
                    .text_color(rgb(INK_SOFT))
                    .child("改动过大，其余部分未显示"),
            );
        }
        let icon = match tool {
            Some(card) => tool_status_icon(card),
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
            .children(tool.map(|card| {
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
        self.save_ui_pref(move |s| s.view_mode = mode);
        cx.notify();
    }

    /// 往上翻过之后，对话区底部居中的「回到底部」圆钮；底下来了新内容时右上角带个赤陶点。
    /// 点一下跳回底部并恢复自动跟随。
    pub(super) fn render_jump_to_bottom(&self, cx: &mut Context<Self>) -> Option<Div> {
        let s = self.session();
        if !s.scrolled_away() {
            return None;
        }
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
            .on_click(cx.listener(|this, _, _, cx| {
                this.session().follow_tail();
                cx.notify();
            }))
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

    /// 「复制」按钮：点了把 `text(当前会话, idx)` 放进剪贴板，图标短暂变成对勾、文字变「已复制」。
    fn copy_action(
        &self,
        id: impl Into<ElementId>,
        idx: usize,
        cx: &mut Context<Self>,
        text: impl Fn(&Session, usize) -> String + 'static,
    ) -> Stateful<Div> {
        let copied = self.copied == Some((self.session().token, idx, None));
        let (icon, label) =
            if copied { (IconAll::Check, "已复制") } else { (IconAll::Copy, "复制") };
        Self::message_action(id, icon, label).on_click(cx.listener(move |this, _, _, cx| {
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

    /// 用户消息气泡；悬停时下方出现「复制 / 编辑 / 在另一个内核上重跑」。
    fn render_user(
        &self,
        idx: usize,
        text: &SharedString,
        images: &[PathBuf],
        cx: &mut Context<Self>,
    ) -> Div {
        let group: SharedString = format!("user-{idx}").into();
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
            .opacity(0.)
            .child(self.copy_action(("copy-user", idx), idx, cx, |s, idx| {
                match &s.transcript[idx] {
                    ChatItem::User { text, .. } => text.to_string(),
                    _ => String::new(),
                }
            }))
            .child(
                Self::message_action(("edit-user", idx), IconAll::Pencil, "编辑")
                    .tooltip(move |window, cx| Tooltip::new(edit_tip).build(window, cx))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.edit_message(idx, window, cx)),
                    ),
            );
        for kind in others {
            actions = actions.child(
                Self::message_action(
                    ("rerun", idx * 4 + kind.index()),
                    IconAll::RefreshCw,
                    format!("在「{}」上重跑", kind.label()),
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
        div()
            .group(group.clone())
            .v_flex()
            .items_end()
            .gap_1()
            .children(thumbs)
            .children(bubble)
            .child(actions.group_hover(group, |s| s.opacity(1.)))
    }

    pub(super) fn render_item(
        &self,
        idx: usize,
        item: &ChatItem,
        layout: RowLayout,
        cx: &mut Context<Self>,
    ) -> Div {
        let verbose = self.settings.view_mode == ViewMode::Verbose;
        match item {
            ChatItem::User { text, images } => self.render_user(idx, text, images, cx),
            ChatItem::Thinking { text, streaming, open, elapsed_ms, .. } => {
                Self::render_thinking(idx, text, *streaming, *open, *elapsed_ms, verbose, cx)
            }
            ChatItem::Info { text, open } => Self::render_info(idx, text, *open, verbose, cx),
            ChatItem::Tool(card) => Self::render_tool(idx, card, verbose, cx),
            ChatItem::AutoApproved { title, chosen } => {
                Self::render_auto_approved(idx, title, chosen)
            }
            ChatItem::Permission { title, options, resolved, .. } => {
                Self::render_permission(idx, title, options, resolved, cx)
            }
            ChatItem::Plan(rows) => Self::render_plan(rows, layout.compact),
            ChatItem::Agent(text) => {
                let s = self.session();
                let streaming = s.busy && idx + 1 == s.transcript.len();
                let text =
                    if streaming { close_dangling_code(text).into_owned() } else { text.clone() };
                // 代码块右上角：语言名 + 「复制」（点完短暂变对勾，和消息下方的复制同一套状态）
                let token = s.token;
                let copied_block = match self.copied {
                    Some((t, i, block)) if t == token && i == idx => block,
                    _ => None,
                };
                let app = cx.entity().downgrade();
                let body = div().min_w_0().text_size(TEXT_LARGE).line_height(LEADING).child(
                    TextView::markdown(("agent", idx), SharedString::from(text))
                        .style(markdown_style())
                        .stream_fade(true)
                        .code_block_actions(move |block, _, _| {
                            let start = block.span.map(|span| span.start).unwrap_or(0);
                            let code = block.code();
                            let app = app.clone();
                            code_block_header(
                                block.lang(),
                                copied_block == Some(start),
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
                            )
                        }),
                );
                if !s.is_turn_end(idx) {
                    return body;
                }
                // 一轮回复的末尾挂「复制」和这一轮的用时 / 用量：最后一轮常显，
                // 更早的悬停才出现（只切透明度，不挤布局）
                let group: SharedString = format!("agent-{idx}").into();
                let latest =
                    !s.transcript[idx + 1..].iter().any(|i| matches!(i, ChatItem::User { .. }));
                let meta = s.turn_summary(idx).and_then(super::status::turn_meta);
                let actions = div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .pt_2()
                    .child(
                        self.copy_action(("copy-agent", idx), idx, cx, |s, idx| s.turn_text(idx)),
                    )
                    .children(meta.map(|m| {
                        // 和运行中的状态行同一套数字字形，一轮结束换过来时不变样
                        div()
                            .text_size(TEXT_META)
                            .text_color(rgb(INK_SOFT))
                            .font_features(tabular_nums())
                            .child(m)
                    }))
                    .when(!latest, |d| d.opacity(0.).group_hover(group.clone(), |d| d.opacity(1.)));
                div().group(group).v_flex().min_w_0().child(body).child(actions)
            }
            ChatItem::Failed { message, overflow } => {
                self.render_failed(idx, message, *overflow, cx)
            }
            ChatItem::Compaction { state, open } => Self::render_compaction(idx, state, *open, cx),
            // 一轮层面的提示（续接、停止…）：和正文左对齐，图标打头
            ChatItem::Note(text) => {
                step_line(Icon::new(IconAll::Info).size_3p5().text_color(rgb(INK_SOFT)))
                    .child(div().flex_1().min_w_0().child(text.clone()))
            }
        }
    }
}

/// 流式输出停在半截代码标记上时（`` `foo `` 或没收尾的 ``` 围栏），渲染前临时补齐，
/// 免得反引号原样露出来、等收尾后整段又跳成代码样式。
fn close_dangling_code(text: &str) -> Cow<'_, str> {
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
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn dangling_code_is_closed_while_streaming() {
        assert_eq!(close_dangling_code("事件来自 ACP 的 `sess"), "事件来自 ACP 的 `sess`");
        assert_eq!(close_dangling_code("用 `a` 和 `b`"), "用 `a` 和 `b`");
        assert_eq!(close_dangling_code("```rust\nfn a() {"), "```rust\nfn a() {\n```");
        // 上一段的单个反引号不影响下一段
        assert_eq!(close_dangling_code("a ` b\n\nc"), "a ` b\n\nc");
        assert_eq!(close_dangling_code("```\n`\n```\nok"), "```\n`\n```\nok");
    }
}
