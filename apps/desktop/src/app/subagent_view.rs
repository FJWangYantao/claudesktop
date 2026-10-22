//! 子任务（自研内核的 `task` 调用）：对话里的子任务卡片，和点开后的详情页。
//!
//! 子 agent 的思考、回复和工具调用不进对话记录，按 task 调用 ID 收在 `Session::subagents` 里
//! （见 `session.rs` 的 `SubagentRun`）。卡片固定两行（标题 / 现在在做什么），内容变了行高不变，
//! 所以不用并进 `Session::row_key`。详情页占内容区（输入框照常在下面），Esc 或「返回」回到对话。

use super::ChatApp;
use super::chat::{STEP_INDENT, ellipsis, icon_cell, markdown_style, reading_markdown, step_line};
use super::motion::{self, Reveal};
use super::widgets::Surface;
use crate::model::*;
use crate::session::SubagentRun;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::text::TextView;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 内置角色的名字（`crates/builtin` 的 `subagent::GENERAL` / `EXPLORE`），界面上叫「通用」「调研」
const GENERAL: &str = "general-purpose";
const EXPLORE: &str = "explore";

/// 卡片 / 详情页标题：task 的说明（标题里带的「（角色）」去掉，角色另画成标签）。
fn task_title(card: &ToolCard, agent: &str) -> SharedString {
    let arg = card.arg.trim();
    let arg = (!agent.is_empty())
        .then(|| arg.strip_suffix(&format!("（{agent}）")))
        .flatten()
        .unwrap_or(arg);
    if arg.is_empty() { "子任务".into() } else { arg.to_string().into() }
}

/// 角色标签上的字；不知道角色（旧日志）时为 None。
fn agent_label(agent: &str) -> Option<SharedString> {
    match agent {
        "" => None,
        GENERAL => Some("通用".into()),
        EXPLORE => Some("调研".into()),
        name => Some(name.to_string().into()),
    }
}

/// 卡片第二行：跑着时是现在在做什么，完了是结果摘要；后面带上调用了几次工具。
fn task_progress(card: &ToolCard, run: Option<&SubagentRun>) -> String {
    let steps = run.map_or(0, SubagentRun::steps);
    let doing = match card.phase {
        ToolPhase::Running => match run.and_then(|r| r.items.last()) {
            Some(ChatItem::Thinking { streaming: true, .. }) => "思考中…".to_string(),
            Some(ChatItem::Agent(_)) => "正在写报告…".to_string(),
            Some(ChatItem::Tool(c)) if c.arg.is_empty() => c.name.to_string(),
            Some(ChatItem::Tool(c)) => format!("{} {}", c.name, c.arg),
            _ => "准备中…".to_string(),
        },
        ToolPhase::Done if card.result.is_empty() => "已完成".to_string(),
        ToolPhase::Failed if card.result.is_empty() => "失败".to_string(),
        _ => card.result.to_string(),
    };
    if steps == 0 { doing } else { format!("{doing} · {steps} 次工具调用") }
}

/// 状态图标：跑着转圈，失败红叉，完成是机器人。
fn task_icon(card: &ToolCard, settle: f32) -> AnyElement {
    let icon = match card.phase {
        ToolPhase::Running => return motion::spinner().into_any_element(),
        ToolPhase::Failed => Icon::new(IconAll::CircleX).size_3p5().text_color(rgb(RED)),
        ToolPhase::Done => Icon::new(IconAll::Bot).size_3p5().text_color(rgb(INK_SOFT)),
    };
    let scale = 0.6 + 0.4 * settle;
    div()
        .opacity(settle)
        .child(icon.transform(Transformation::scale(size(scale, scale))))
        .into_any_element()
}

/// 角色标签：沙色小底。
fn agent_tag(label: SharedString) -> Div {
    div()
        .flex_shrink_0()
        .px_1p5()
        .rounded_sm()
        .bg(rgb(SAND))
        .text_size(TEXT_META)
        .text_color(rgb(INK_SOFT))
        .child(label)
}

/// 详情页里的小节标题。
fn section_label(text: &'static str) -> Div {
    div()
        .text_size(TEXT_SMALL)
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(INK_SOFT))
        .child(text)
}

impl ChatApp {
    /// 对话里的子任务卡片：整块可点，点开详情页。
    pub(super) fn render_task_card(
        &self,
        idx: usize,
        card: &ToolCard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let task = card.id.clone().unwrap_or_default();
        let run = self.session().subagents.get(&task);
        let agent = run.map(|r| r.agent.clone()).unwrap_or_default();
        let settle = motion::settle(("tool", idx), card.phase != ToolPhase::Running, window, cx);
        let failed = card.phase == ToolPhase::Failed;
        let head = div()
            .h_flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(icon_cell(task_icon(card, settle)))
            .child(
                ellipsis(("task-title", idx), task_title(card, &agent))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK)),
            )
            .children(agent_label(&agent).map(agent_tag))
            .child(div().flex_1())
            .child(
                div()
                    .flex_shrink_0()
                    .h_flex()
                    .items_center()
                    .gap_0p5()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child("查看过程")
                    .child(Icon::new(IconAll::ChevronRight).size_3()),
            );
        let progress = div()
            .pl(STEP_INDENT)
            .min_w_0()
            .text_size(TEXT_SMALL)
            .text_color(rgb(if failed { RED } else { INK_SOFT }))
            .child(ellipsis(("task-progress", idx), task_progress(card, run).into()));
        div().child(
            div()
                .id(("task-card", idx))
                .v_flex()
                .gap_0p5()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(CARD))
                .text_size(TEXT_BODY)
                .cursor_pointer()
                .hover(|d| d.border_color(rgb(LINE_STRONG)))
                .on_click(cx.listener(move |this, _, _, cx| this.open_task(task.clone(), cx)))
                .child(head)
                .child(progress),
        )
    }

    /// 打开当前会话里某个子任务的详情页。
    pub(super) fn open_task(&mut self, task: String, cx: &mut Context<Self>) {
        let active = self.active;
        self.sessions[active].viewing_task = Some(task);
        self.task_scroll = ScrollHandle::new();
        cx.notify();
    }

    /// 从详情页回到对话。
    pub(super) fn close_task(&mut self, cx: &mut Context<Self>) {
        let active = self.active;
        self.sessions[active].viewing_task = None;
        cx.notify();
    }

    /// 子任务详情页：任务说明、子 agent 的过程（思考 / 工具 / 中间的回复）、最后交回的报告。
    /// 对应的 task 行已经不在了（回退过）时返回 None，调用方改画对话。
    pub(super) fn render_task_page(
        &self,
        task: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let s = self.session();
        let card = s.transcript.iter().rev().find_map(|i| match i {
            ChatItem::Tool(c) if c.task && c.id.as_deref() == Some(task) => Some(c),
            _ => None,
        })?;
        let run = s.subagents.get(task)?;
        let running = card.phase == ToolPhase::Running;
        let failed = card.phase == ToolPhase::Failed;

        let status: SharedString = match card.phase {
            ToolPhase::Running => "进行中".into(),
            ToolPhase::Done => "已完成".into(),
            ToolPhase::Failed if card.result.is_empty() => "失败".into(),
            ToolPhase::Failed => format!("失败：{}", card.result).into(),
        };
        let steps = run.steps();
        let header = div()
            .h_flex()
            .items_start()
            .gap_2()
            .child(
                Self::quiet_button("task-back", IconAll::ArrowLeft, Surface::Paper, cx)
                    .small()
                    .tooltip("返回对话")
                    .on_click(cx.listener(|this, _, _, cx| this.close_task(cx))),
            )
            .child(
                div()
                    .v_flex()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_size(TEXT_LEAD)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(INK))
                            .child(task_title(card, &run.agent)),
                    )
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .children(agent_label(&run.agent).map(agent_tag))
                            .when(running, |d| d.child(motion::spinner()))
                            .child(div().when(failed, |d| d.text_color(rgb(RED))).child(status))
                            .when(steps > 0, |d| {
                                d.child(
                                    div()
                                        .font_features(tabular_nums())
                                        .child(format!("· {steps} 次工具调用")),
                                )
                            }),
                    ),
            );

        let mut page = div().v_flex().gap_5().child(header);

        // 子任务里的调用在等授权：权限卡直接摆在这里（按钮、Alt+数字照常管用），不用先回对话
        if let Some(ix) = s.pending_permission()
            && let Some(ChatItem::Permission {
                title,
                options,
                resolved,
                tool: Some(tool),
                hunks,
                keep,
                ..
            }) = s.transcript.get(ix)
            && tool.starts_with(&format!("{task}/"))
        {
            page = page.child(Self::render_permission(
                ix,
                title,
                options,
                resolved,
                hunks.as_ref(),
                keep.as_ref(),
                true,
                cx,
            ));
        }

        if !run.prompt.is_empty() {
            page = page.child(
                div().v_flex().gap_2().child(section_label("任务说明")).child(
                    div()
                        .px_4()
                        .py_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(LINE))
                        .bg(rgb(CARD))
                        .child(
                            TextView::markdown("task-prompt", reading_markdown(&run.prompt, false))
                                .style(markdown_style())
                                .markdown_extensions(crate::app::math::extensions())
                                .text_size(TEXT_BODY)
                                .text_color(rgb(INK))
                                .line_height(LEADING),
                        ),
                ),
            );
        }

        // 报告：最后一段回复，子任务结束了才算（跑着时它还可能接着调工具）
        let report = (!running).then(|| run.report()).flatten().map(|_| run.items.len() - 1);
        let mut process = div().v_flex().gap_3().child(section_label("过程"));
        let mut shown = 0;
        for (k, item) in run.items.iter().enumerate() {
            if Some(k) == report {
                break;
            }
            let row = match item {
                ChatItem::Thinking { text, streaming, open, .. } => {
                    Self::render_task_thought(task, k, text, *streaming, *open, window, cx)
                }
                ChatItem::Tool(c) => Self::render_task_tool(task, k, c, &s.cwd, window, cx),
                ChatItem::Agent(text) => {
                    let streaming = running && k + 1 == run.items.len();
                    div().child(
                        TextView::markdown(("task-text", k), reading_markdown(text, streaming))
                            .style(markdown_style())
                            .markdown_extensions(crate::app::math::extensions())
                            .text_size(TEXT_BODY)
                            .text_color(rgb(INK))
                            .line_height(LEADING),
                    )
                }
                _ => continue,
            };
            process = process.child(row);
            shown += 1;
        }
        // 跑着、最后一行又不在动（还没开始，或刚调完工具在等模型）时补一行转圈，让人知道没停
        let idle = match run.items.last() {
            None => true,
            Some(ChatItem::Tool(c)) => c.phase != ToolPhase::Running,
            Some(_) => false,
        };
        if running && idle {
            let label = if run.items.is_empty() { "准备中…" } else { "等模型下一步…" };
            process = process.child(step_line(motion::spinner()).child(label));
        } else if !running && shown == 0 {
            process = process
                .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child("没有中间过程"));
        }
        page = page.child(process);

        if let Some(k) = report
            && let Some(ChatItem::Agent(text)) = run.items.get(k)
        {
            page = page.child(
                div().v_flex().gap_2().child(section_label("交回的报告")).child(
                    TextView::markdown("task-report", reading_markdown(text, false))
                        .style(markdown_style())
                        .text_size(TEXT_LARGE)
                        .text_color(rgb(INK))
                        .line_height(LEADING),
                ),
            );
        }

        Some(
            div()
                .id("task-page")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.task_scroll)
                .child(div().max_w(READING_W).mx_auto().w_full().px_6().py_6().child(page))
                .into_any_element(),
        )
    }

    /// 详情页里的一段思考：一行，点开看全文（同对话里的思考块，只是状态记在子任务里）。
    #[allow(clippy::too_many_arguments)]
    fn render_task_thought(
        task: &str,
        k: usize,
        text: &str,
        streaming: bool,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let has_text = !text.trim().is_empty();
        let task_id = task.to_string();
        let header = step_line(Icon::new(IconAll::Brain).size_3p5().text_color(rgb(INK_SOFT)))
            .id(("task-think-toggle", k))
            .when(has_text, |d| {
                d.cursor_pointer().hover(|d| d.text_color(rgb(INK))).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let active = this.active;
                        if let Some(ChatItem::Thinking { open, .. }) = this.sessions[active]
                            .subagents
                            .get_mut(&task_id)
                            .and_then(|r| r.items.get_mut(k))
                        {
                            *open = !*open;
                            cx.notify();
                        }
                    },
                ))
            })
            .child(if streaming {
                motion::shimmer_text("思考中…", INK_SOFT, INK).into_any_element()
            } else {
                div().child("思考").into_any_element()
            })
            .when(has_text, |d| {
                d.child(
                    Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                        .size_3(),
                )
            });
        let mut col = div().v_flex().child(header);
        let progress = motion::reveal_progress(("task-think", k), open && has_text, window, cx);
        if progress > 0. && has_text {
            let body = div()
                .pt_1()
                .pl(STEP_INDENT)
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .line_height(LEADING)
                .child(text.trim().to_string());
            col = col.child(Reveal::new(("task-think-body", k), progress, body));
        }
        col
    }

    /// 详情页里的一次工具调用：改了文件的画 diff，其余是工具行、有输出时点开看。
    fn render_task_tool(
        task: &str,
        k: usize,
        card: &ToolCard,
        cwd: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let settle = motion::settle(("task-tool", k), card.phase != ToolPhase::Running, window, cx);
        if !card.diffs.is_empty() {
            return div().v_flex().gap_2().children(card.diffs.iter().map(|diff| {
                Self::render_diff(diff, cwd, Some(DIFF_PREVIEW_ROWS), None, Some((card, settle)))
            }));
        }
        let has_output = card.can_open();
        let task_id = task.to_string();
        // 元素 ID 和对话里的工具行错开（详情页和对话不同时画，但下标空间不同）
        let line = Self::render_tool_line(100_000 + k, card, settle)
            .id(("task-tool-toggle", k))
            .when(has_output, |d| {
                d.cursor_pointer()
                    .hover(|d| d.text_color(rgb(INK)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let active = this.active;
                        if let Some(ChatItem::Tool(card)) = this.sessions[active]
                            .subagents
                            .get_mut(&task_id)
                            .and_then(|r| r.items.get_mut(k))
                        {
                            card.expanded = !card.expanded;
                            cx.notify();
                        }
                    }))
                    .child(
                        div().flex_shrink_0().child(
                            Icon::new(if card.expanded {
                                IconAll::ChevronDown
                            } else {
                                IconAll::ChevronRight
                            })
                            .size_3(),
                        ),
                    )
            });
        let mut col = div().v_flex().child(line);
        let progress =
            motion::reveal_progress(("task-tool", k), has_output && card.expanded, window, cx);
        if progress > 0. && has_output {
            col = col.child(Reveal::new(("task-tool-body", k), progress, Self::tool_output(card)));
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(arg: &str, phase: ToolPhase, result: &str) -> ToolCard {
        ToolCard {
            id: Some("t1".into()),
            kind: "think".into(),
            name: "Task".into(),
            arg: arg.to_string().into(),
            result: result.to_string().into(),
            output: SharedString::default(),
            phase,
            diffs: Vec::new(),
            expanded: false,
            task: true,
            grant: None,
        }
    }

    #[core::prelude::v1::test]
    fn title_drops_the_agent_suffix() {
        let c = card("找秘密（scout）", ToolPhase::Running, "");
        assert_eq!(task_title(&c, "scout").as_ref(), "找秘密");
        // 不知道角色时原样显示
        assert_eq!(task_title(&c, "").as_ref(), "找秘密（scout）");
        assert_eq!(task_title(&card("", ToolPhase::Running, ""), "").as_ref(), "子任务");
        assert_eq!(agent_label(GENERAL).as_deref(), Some("通用"));
        assert_eq!(agent_label(EXPLORE).as_deref(), Some("调研"));
        assert_eq!(agent_label(""), None);
    }

    #[core::prelude::v1::test]
    fn progress_says_what_it_is_doing_then_the_result() {
        let mut run = SubagentRun::default();
        let running = card("找", ToolPhase::Running, "");
        assert_eq!(task_progress(&running, Some(&run)), "准备中…");
        let mut read = card("notes.txt", ToolPhase::Running, "");
        read.name = "Read".into();
        run.items.push(ChatItem::Tool(read));
        assert_eq!(task_progress(&running, Some(&run)), "Read notes.txt · 1 次工具调用");
        run.items.push(ChatItem::Agent("秘密是 42".into()));
        assert_eq!(task_progress(&running, Some(&run)), "正在写报告… · 1 次工具调用");
        let done = card("找", ToolPhase::Done, "秘密是 42");
        assert_eq!(task_progress(&done, Some(&run)), "秘密是 42 · 1 次工具调用");
        assert_eq!(task_progress(&card("找", ToolPhase::Failed, ""), None), "失败");
    }
}
