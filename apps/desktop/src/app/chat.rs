//! 对话记录渲染：思考块、工具行、权限卡、diff、计划、正文。

use super::ChatApp;
use crate::model::*;
use crate::theme::*;
use claudesktop_core::PermissionOptionView;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

impl ChatApp {
    pub(super) fn render_thinking(
        idx: usize,
        text: &str,
        streaming: bool,
        open: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let caret = if streaming { " ▍" } else { "" };
        let header = div()
            .id(("think-toggle", idx))
            .h_flex()
            .gap_1p5()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(ChatItem::Thinking { open, .. }) = this.sessions[this.active].transcript.get_mut(idx) {
                    *open = !*open;
                    cx.notify();
                }
            }))
            .child(
                Icon::new(IconAll::Brain)
                    .size_3p5()
                    .text_color(rgb(INK_SOFT)),
            )
            .child(
                div().text_size(px(12.5)).text_color(rgb(INK_SOFT)).child(if streaming {
                    SharedString::from("思考中…")
                } else {
                    SharedString::from("已思考")
                }),
            )
            .child(
                Icon::new(if open {
                    IconAll::ChevronDown
                } else {
                    IconAll::ChevronRight
                })
                .size_3()
                .text_color(rgb(INK_SOFT)),
            );

        let mut col = div().v_flex().gap_1().child(header);
        if open {
            col = col.child(
                div()
                    .text_size(px(13.))
                    .text_color(rgb(INK_SOFT))
                    .line_height(relative(1.6))
                    .child(format!("{text}{caret}")),
            );
        }
        col
    }

    pub(super) fn render_info(idx: usize, text: &str, open: bool, cx: &mut Context<Self>) -> Div {
        let header = div()
            .id(("info-toggle", idx))
            .h_flex()
            .gap_1p5()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(ChatItem::Info { open, .. }) = this.sessions[this.active].transcript.get_mut(idx) {
                    *open = !*open;
                    cx.notify();
                }
            }))
            .child(Icon::new(IconAll::Info).size_3p5().text_color(rgb(INK_SOFT)))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(INK_SOFT))
                    .child("启动信息"),
            )
            .child(
                Icon::new(if open {
                    IconAll::ChevronDown
                } else {
                    IconAll::ChevronRight
                })
                .size_3()
                .text_color(rgb(INK_SOFT)),
            );
        let mut col = div().v_flex().gap_1().child(header);
        if open {
            col = col.child(
                div()
                    .text_size(px(13.))
                    .font_family(MONO)
                    .text_color(rgb(INK_SOFT))
                    .line_height(relative(1.6))
                    .child(text.to_string()),
            );
        }
        col
    }

    pub(super) fn render_tool(card: &ToolCard) -> Div {
        let dot = match card.phase {
            ToolPhase::Running => CORAL,
            ToolPhase::Done => GREEN,
            ToolPhase::Failed => RED,
        };
        let mut col = div().v_flex().gap(px(2.)).child(
            div()
                .h_flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .size(px(7.))
                        .mt(px(7.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(rgb(dot)),
                )
                .child(
                    div()
                        .text_size(px(14.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(INK))
                        .child(card.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.5))
                        .font_family(MONO)
                        .text_color(rgb(INK_SOFT))
                        .line_height(relative(1.7))
                        .child(card.arg.clone()),
                ),
        );
        if !card.result.is_empty() {
            col = col.child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_2()
                    .pl_3()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_family(MONO)
                            .text_color(rgb(INK_FAINT))
                            .child("└"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.5))
                            .font_family(MONO)
                            .text_color(rgb(INK_SOFT))
                            .child(card.result.clone()),
                    ),
            );
        }
        col
    }

    pub(super) fn render_permission(
        idx: usize,
        title: &SharedString,
        options: &[PermissionOptionView],
        resolved: &Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> Div {
        let header = div()
            .h_flex()
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconAll::ShieldAlert)
                    .size_4()
                    .text_color(rgb(CORAL)),
            )
            .child(
                div()
                    .text_size(px(14.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("请求权限"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.5))
                    .font_family(MONO)
                    .text_color(rgb(INK_SOFT))
                    .child(title.clone()),
            );

        let footer = match resolved {
            Some(label) => div()
                .text_size(px(13.5))
                .text_color(rgb(INK_SOFT))
                .child(format!("已选择：{label}")),
            None => {
                let mut row = div().h_flex().gap_2();
                for (n, opt) in options.iter().enumerate() {
                    let allow = opt.kind.starts_with("allow");
                    let primary = opt.kind == "allow_once";
                    let (id, name) = (opt.id.clone(), opt.name.clone());
                    row = row.child(
                        div()
                            .id(("perm", idx * 16 + n))
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .cursor_pointer()
                            .text_size(px(13.5))
                            .when(primary, |d| d.bg(rgb(CORAL)).text_color(rgb(0xFFFFFF)))
                            .when(!primary, |d| {
                                d.border_1()
                                    .border_color(rgb(LINE))
                                    .bg(rgb(CARD))
                                    .hover(|d| d.bg(rgb(SAND)))
                                    .text_color(rgb(if allow { INK } else { RED }))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.resolve_permission(
                                    idx,
                                    Some((id.clone(), name.clone())),
                                    cx,
                                );
                            }))
                            .child(opt.name.clone()),
                    );
                }
                row
            }
        };

        div()
            .v_flex()
            .gap_2p5()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(CORAL))
            .bg(rgb(CARD))
            .child(header)
            .child(footer)
    }

    pub(super) fn render_diff(path: &'static str, rows: DiffRows) -> Div {
        let mut old_no = 1usize;
        let mut new_no = 1usize;
        let mut body = div().v_flex();
        for (kind, text) in rows {
            let (bg, fg, no) = match kind {
                '+' => {
                    let n = new_no;
                    new_no += 1;
                    (Some(DIFF_ADD_BG), DIFF_ADD_FG, n)
                }
                '-' => {
                    let n = old_no;
                    old_no += 1;
                    (Some(DIFF_DEL_BG), DIFF_DEL_FG, n)
                }
                _ => {
                    let n = new_no;
                    old_no += 1;
                    new_no += 1;
                    (None, INK_SOFT, n)
                }
            };
            body = body.child(
                div()
                    .h_flex()
                    .items_start()
                    .when_some(bg, |d, c| d.bg(rgb(c)))
                    .font_family(MONO)
                    .text_size(px(13.5))
                    .line_height(relative(1.6))
                    .child(
                        div()
                            .w(px(36.))
                            .flex_shrink_0()
                            .pr_2()
                            .flex()
                            .justify_end()
                            .text_color(rgb(INK_FAINT))
                            .child(format!("{no}")),
                    )
                    .child(
                        div()
                            .w(px(14.))
                            .flex_shrink_0()
                            .text_color(rgb(fg))
                            .child(kind.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(rgb(if bg.is_some() { fg } else { INK }))
                            .child(*text),
                    ),
            );
        }
        div()
            .v_flex()
            .rounded_lg()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .overflow_hidden()
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .px_3()
                    .py_1p5()
                    .bg(rgb(SAND))
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        Icon::new(IconAll::FileText)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_family(MONO)
                            .text_color(rgb(INK_SOFT))
                            .child(path),
                    ),
            )
            .child(body)
    }

    pub(super) fn render_plan(rows: &[PlanRow]) -> Div {
        div()
            .v_flex()
            .gap_1p5()
            .p_3()
            .rounded_lg()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .child(
                        Icon::new(IconAll::ListTodo)
                            .size_4()
                            .text_color(rgb(CORAL)),
                    )
                    .child(div().text_size(px(14.)).text_color(rgb(INK)).child("计划")),
            )
            .children(rows.iter().enumerate().map(|(i, row)| {
                let marker = if row.done {
                    div()
                        .size(px(15.))
                        .rounded_full()
                        .bg(rgb(GREEN))
                        .flex()
                        .justify_center()
                        .items_center()
                        .child(
                            Icon::new(IconAll::Check)
                                .size_3()
                                .text_color(rgb(0xFFFFFF)),
                        )
                } else {
                    div()
                        .size(px(15.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(LINE))
                };
                div()
                    .id(("plan-row", i))
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(marker)
                    .child(
                        div()
                            .text_size(px(13.5))
                            .text_color(if row.done {
                                rgb(INK_SOFT)
                            } else {
                                rgb(INK)
                            })
                            .child(row.text.clone()),
                    )
            }))
    }

    /// 助手回合行：官方界面没有头像轨道，内容直接顶格。
    pub(super) fn railed(content: impl IntoElement) -> Div {
        div().min_w_0().child(content)
    }

    pub(super) fn render_item(&self, idx: usize, item: &ChatItem, cx: &mut Context<Self>) -> Div {
        match item {
            ChatItem::User(text) => div()
                .w_full()
                .bg(rgb(SAND))
                .text_color(rgb(INK))
                .rounded(px(12.))
                .px_4()
                .py_3()
                .text_size(px(15.))
                .line_height(relative(1.6))
                .child(text.clone()),
            ChatItem::Thinking {
                text,
                streaming,
                open,
            } => Self::railed(Self::render_thinking(idx, text, *streaming, *open, cx)),
            ChatItem::Info { text, open } => Self::railed(Self::render_info(idx, text, *open, cx)),
            ChatItem::Tool(card) => Self::railed(Self::render_tool(card)),
            ChatItem::Diff { path, rows } => Self::railed(Self::render_diff(path, rows)),
            ChatItem::Permission {
                title,
                options,
                resolved,
                ..
            } => Self::railed(Self::render_permission(idx, title, options, resolved, cx)),
            ChatItem::Plan(rows) => Self::railed(Self::render_plan(rows)),
            ChatItem::Agent(text) => Self::railed(
                div().text_size(px(15.5)).line_height(relative(1.7)).child(
                    TextView::markdown(("agent", idx), SharedString::from(text.clone()))
                        .style(
                            TextViewStyle::default()
                                .paragraph_gap(rems(0.9))
                                .heading_font_size(|level, _| match level {
                                    1 => px(22.),
                                    2 => px(19.),
                                    3 => px(17.),
                                    _ => px(15.5),
                                }),
                        )
                        .stream_fade(true),
                ),
            ),
            ChatItem::Note(text) => {
                div().h_flex().justify_center().pt_1().child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(INK_FAINT))
                        .child(text.clone()),
                )
            }
        }
    }
}
