//! 对话记录渲染：思考块、工具行、权限卡、diff、计划、正文。

use super::ChatApp;
use crate::model::*;
use crate::session::Session;
use crate::settings::{KernelChoice, Settings};
use crate::theme::*;
use claudesktop_core::{DiffView, PermissionOptionView};
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::Position;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::borrow::Cow;
use std::path::PathBuf;
use std::time::Duration;

impl ChatApp {
    /// 思考块；`verbose`（详细视图）时总是展开、不能收起。
    pub(super) fn render_thinking(
        idx: usize,
        text: &str,
        streaming: bool,
        open: bool,
        verbose: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let caret = if streaming { " ▍" } else { "" };
        let open = open || verbose;
        let header = div()
            .id(("think-toggle", idx))
            .h_flex()
            .gap_1p5()
            .when(!verbose, |d| {
                d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(ChatItem::Thinking { open, .. }) =
                        this.sessions[this.active].transcript.get_mut(idx)
                    {
                        *open = !*open;
                        cx.notify();
                    }
                }))
            })
            .child(
                Icon::new(IconAll::Brain)
                    .size_3p5()
                    .text_color(rgb(INK_SOFT)),
            )
            .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(if streaming {
                // 流式中高光扫过；系统开了「减少动画」时组件自己退回静态文字
                ShimmerText::new("思考中…")
                    .id(("think-shimmer", idx))
                    .highlight_color(rgb(INK))
                    .into_any_element()
            } else {
                "已思考".into_any_element()
            }))
            .when(!verbose, |d| {
                d.child(
                    Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                        .size_3()
                        .text_color(rgb(INK_SOFT)),
                )
            });

        let mut col = div().v_flex().gap_1().child(header);
        if open {
            col = col.child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .line_height(LEADING)
                    .child(format!("{text}{caret}")),
            );
        }
        col
    }

    /// 启动信息块；`verbose` 时总是展开。
    pub(super) fn render_info(
        idx: usize,
        text: &str,
        open: bool,
        verbose: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = open || verbose;
        let header = div()
            .id(("info-toggle", idx))
            .h_flex()
            .gap_1p5()
            .when(!verbose, |d| {
                d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(ChatItem::Info { open, .. }) =
                        this.sessions[this.active].transcript.get_mut(idx)
                    {
                        *open = !*open;
                        cx.notify();
                    }
                }))
            })
            .child(Icon::new(IconAll::Info).size_3p5().text_color(rgb(INK_SOFT)))
            .child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child("启动信息"),
            )
            .when(!verbose, |d| {
                d.child(
                    Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                        .size_3()
                        .text_color(rgb(INK_SOFT)),
                )
            });
        let mut col = div().v_flex().gap_1().child(header);
        if open {
            col = col.child(
                div()
                    .text_size(TEXT_SMALL)
                    .font_family(MONO)
                    .text_color(rgb(INK_SOFT))
                    .line_height(LEADING)
                    .child(text.to_string()),
            );
        }
        col
    }

    /// 工具行；`verbose` 时 diff 全部展开、不给展开 / 收起开关。
    pub(super) fn render_tool(
        idx: usize,
        card: &ToolCard,
        verbose: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        // 官方样式：工具名加粗 + 参数小药丸 + 改动行数；完成时不再挂状态点
        let status = match card.phase {
            ToolPhase::Running => Some(
                Spinner::new()
                    .xsmall()
                    .color(rgb(CORAL).into())
                    .into_any_element(),
            ),
            ToolPhase::Failed => Some(
                Icon::new(IconAll::X)
                    .size_3p5()
                    .text_color(rgb(RED))
                    .into_any_element(),
            ),
            ToolPhase::Done => None,
        };
        let (added, removed) = card
            .diffs
            .iter()
            .fold((0, 0), |(a, r), d| (a + d.added, r + d.removed));
        let arg = card.arg.clone();
        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(TEXT_BODY)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(INK))
                    .child(card.name.clone()),
            )
            .when(!arg.is_empty(), |d| {
                d.child(
                    div()
                        .id(("tool-arg", idx))
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .px_1p5()
                        .rounded_sm()
                        .bg(rgb(CODE_BG))
                        .font_family(MONO)
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .tooltip(move |window, cx| Tooltip::new(arg.clone()).build(window, cx))
                        .child(card.arg.clone()),
                )
            })
            .when(added + removed > 0, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .h_flex()
                        .gap_1()
                        .font_family(MONO)
                        .text_size(TEXT_META)
                        .child(div().text_color(rgb(DIFF_ADD_FG)).child(format!("+{added}")))
                        .child(div().text_color(rgb(DIFF_DEL_FG)).child(format!("−{removed}"))),
                )
            })
            .children(status);
        let mut col = div().v_flex().gap_1().child(header);
        if !card.result.is_empty() {
            col = col.child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_1p5()
                    .pl_1()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .line_height(LEADING)
                    .text_color(rgb(INK_SOFT))
                    .child("└")
                    .child(div().flex_1().min_w_0().child(card.result.clone())),
            );
        }
        // 文件改动：默认每份只显示前若干行，超出给一个展开开关
        let limit = (!card.expanded && !verbose).then_some(DIFF_PREVIEW_ROWS);
        for diff in &card.diffs {
            col = col.child(div().mt_1().child(Self::render_diff(diff, limit)));
        }
        let hidden: usize = card
            .diffs
            .iter()
            .map(|d| d.lines.len().saturating_sub(DIFF_PREVIEW_ROWS))
            .sum();
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
                    .text_size(TEXT_LARGE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("要允许这一步吗？"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(TEXT_BODY)
                    .font_family(MONO)
                    .text_color(rgb(INK_SOFT))
                    .child(title.clone()),
            );

        let footer = match resolved {
            Some(label) => div()
                .text_size(TEXT_BODY)
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
                            .text_size(TEXT_BODY)
                            .when(primary, |d| {
                                d.bg(rgb(CORAL_DEEP))
                                    .text_color(rgb(WHITE))
                                    .hover(|d| d.bg(rgb(CORAL_DEEP_HOVER)))
                            })
                            .when(!primary, |d| {
                                d.border_1()
                                    .border_color(rgb(LINE))
                                    .bg(rgb(CARD))
                                    .hover(|d| d.bg(rgb(HOVER)))
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

    /// 一个文件的 diff 卡片；`limit` = 最多显示的行数（None = 全部）。
    pub(super) fn render_diff(diff: &DiffView, limit: Option<usize>) -> Div {
        let shown = limit.map_or(diff.lines.len(), |n| n.min(diff.lines.len()));
        let mut body = div().v_flex();
        for line in &diff.lines[..shown] {
            if line.tag == '~' {
                body = body.child(
                    div()
                        .h(px(18.))
                        .pl(px(44.))
                        .font_family(MONO)
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
                    .font_family(MONO)
                    .text_size(TEXT_BODY)
                    .line_height(LEADING)
                    .child(
                        div()
                            .w(px(44.))
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
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child("改动过大，其余部分未显示"),
            );
        }
        let mut header = div()
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
                    .flex_1()
                    .min_w_0()
                    .text_size(TEXT_SMALL)
                    .font_family(MONO)
                    .text_color(rgb(INK_SOFT))
                    .child(diff.path.clone()),
            );
        if diff.created {
            header = header.child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child("新文件"));
        }
        header = header
            .child(
                div()
                    .text_size(TEXT_SMALL)
                    .font_family(MONO)
                    .text_color(rgb(DIFF_ADD_FG))
                    .child(format!("+{}", diff.added)),
            )
            .child(
                div()
                    .text_size(TEXT_SMALL)
                    .font_family(MONO)
                    .text_color(rgb(DIFF_DEL_FG))
                    .child(format!("−{}", diff.removed)),
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

    pub(super) fn render_plan(rows: &[PlanRow]) -> Div {
        let done = rows.iter().filter(|r| r.done).count();
        div()
            .v_flex()
            .gap_1p5()
            .pl_3()
            .py_0p5()
            .border_l_2()
            .border_color(rgb(CORAL))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(TEXT_SMALL)
                    .child(Icon::new(IconAll::ListTodo).size_3p5().text_color(rgb(CORAL)))
                    .child(div().text_color(rgb(INK_SOFT)).child("计划"))
                    .child(
                        div()
                            .text_color(rgb(INK_SOFT))
                            .child(format!("{done}/{}", rows.len())),
                    ),
            )
            .children(rows.iter().enumerate().map(|(i, row)| {
                let marker = if row.done {
                    div()
                        .size(px(14.))
                        .rounded_full()
                        .bg(rgb(GREEN))
                        .flex()
                        .justify_center()
                        .items_center()
                        .child(Icon::new(IconAll::Check).size_2p5().text_color(rgb(WHITE)))
                } else {
                    div()
                        .size(px(14.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(INK_FAINT))
                };
                div()
                    .id(("plan-row", i))
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(marker.flex_shrink_0())
                    .child(
                        div()
                            .text_size(TEXT_BODY)
                            .text_color(rgb(if row.done { INK_SOFT } else { INK }))
                            .child(row.text.clone()),
                    )
            }))
    }

    /// 切换视图模式并写回设置文件。只改盘上那份的这一项：内存里的设置合并过环境变量，不能整份写回。
    pub(crate) fn set_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        if self.settings.view_mode == mode {
            return;
        }
        self.settings.view_mode = mode;
        if let Some(path) = &self.settings_path {
            let (mut on_disk, broken) = Settings::load(Some(path));
            // 文件坏了就不碰，免得用默认值盖掉
            if broken.is_none() {
                on_disk.view_mode = mode;
                if let Err(err) = on_disk.save(path) {
                    self.sessions[self.active].note(format!("视图模式没能保存：{err}"));
                }
            }
        }
        cx.notify();
    }

    /// 「复制」按钮：点了把 `text(当前会话, idx)` 放进剪贴板，图标短暂变成对勾、文字变「已复制」。
    fn copy_action(
        &self,
        id: impl Into<ElementId>,
        idx: usize,
        cx: &mut Context<Self>,
        text: impl Fn(&Session, usize) -> String + 'static,
    ) -> Stateful<Div> {
        let copied = self.copied == Some((self.session().token, idx));
        let (icon, label) = if copied { (IconAll::Check, "已复制") } else { (IconAll::Copy, "复制") };
        Self::message_action(id, icon, label).on_click(cx.listener(move |this, _, _, cx| {
            let content = text(this.session(), idx);
            this.copy_to_clipboard(idx, content, cx);
        }))
    }

    /// 写剪贴板并记下「刚复制的是哪条」，1.5 秒后自动清掉对勾。
    pub(crate) fn copy_to_clipboard(&mut self, idx: usize, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        let key = (self.session().token, idx);
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
            .child(self.copy_action(("copy-user", idx), idx, cx, |s, idx| match &s.transcript[idx] {
                ChatItem::User { text, .. } => text.to_string(),
                _ => String::new(),
            }))
            .child(
                Self::message_action(("edit-user", idx), IconAll::Pencil, "编辑")
                    .tooltip(move |window, cx| Tooltip::new(edit_tip).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| this.edit_message(idx, window, cx))),
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
            Some(
                div()
                    .h_flex()
                    .justify_end()
                    .gap_1p5()
                    .flex_wrap()
                    .max_w(relative(0.85))
                    .children(images.iter().filter(|p| p.exists()).map(|p| {
                        img(p.clone())
                            .size_20()
                            .rounded_md()
                            .object_fit(ObjectFit::Cover)
                    })),
            )
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

    /// 助手回合行：官方界面没有头像轨道，内容直接顶格。
    pub(super) fn railed(content: impl IntoElement) -> Div {
        div().min_w_0().child(content)
    }

    pub(super) fn render_item(&self, idx: usize, item: &ChatItem, cx: &mut Context<Self>) -> Div {
        let verbose = self.settings.view_mode == ViewMode::Verbose;
        match item {
            ChatItem::User { text, images } => self.render_user(idx, text, images, cx),
            ChatItem::Thinking {
                text,
                streaming,
                open,
            } => Self::railed(Self::render_thinking(idx, text, *streaming, *open, verbose, cx)),
            ChatItem::Info { text, open } => {
                Self::railed(Self::render_info(idx, text, *open, verbose, cx))
            }
            ChatItem::Tool(card) => Self::railed(Self::render_tool(idx, card, verbose, cx)),
            ChatItem::Permission {
                title,
                options,
                resolved,
                ..
            } => Self::railed(Self::render_permission(idx, title, options, resolved, cx)),
            ChatItem::Plan(rows) => Self::railed(Self::render_plan(rows)),
            ChatItem::Agent(text) => {
                let s = self.session();
                let streaming = s.busy && idx + 1 == s.transcript.len();
                let text = if streaming {
                    close_dangling_code(text).into_owned()
                } else {
                    text.clone()
                };
                let body = div().text_size(TEXT_LARGE).line_height(LEADING).child(
                    TextView::markdown(("agent", idx), SharedString::from(text))
                        .style(
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
                                .code_block(StyleRefinement::default().bg(rgb(SAND))),
                        )
                        .stream_fade(true),
                );
                if !s.is_turn_end(idx) {
                    return Self::railed(body);
                }
                // 一轮回复的末尾挂「复制」：最后一轮常显，更早的悬停才出现（只切透明度，不挤布局）
                let group: SharedString = format!("agent-{idx}").into();
                let latest = !s.transcript[idx + 1..]
                    .iter()
                    .any(|i| matches!(i, ChatItem::User { .. }));
                let actions = div()
                    .h_flex()
                    .pt_2()
                    .child(self.copy_action(("copy-agent", idx), idx, cx, |s, idx| s.turn_text(idx)))
                    .when(!latest, |d| d.opacity(0.).group_hover(group.clone(), |d| d.opacity(1.)));
                Self::railed(div().group(group).v_flex().child(body).child(actions))
            }
            ChatItem::Note(text) => {
                div().h_flex().justify_center().pt_1().child(
                    div()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .child(text.clone()),
                )
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
