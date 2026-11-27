//! 对话流末尾的运行状态行：本轮在干什么、跑了多久、正在下发多少 token、怎么停。
//! 指示器使用极简几何图标或统一的弹性双块，不再使用卡通表情。
//! 数据来自 `Session::activity` / `turn_usage` / `turn_started` / `live_output_tokens`
//! （后者是本轮输出量：端点报过的 + 还在流式、靠字数估的，见 `usage::Streamed`）。
//! 数出来的和估出来的得分得出来：端点报过用量是实色的 `↓ 1.2k token`，
//! 还没报过是次要色的 `≈ 1.2k token`（见 [`live_usage_label`]）。
//! 一轮结束后用时和用量挂到回复末尾（[`turn_meta`]）。

use super::ChatApp;
use super::mascot::Mood;
use super::motion::{mascot, shimmer_text};
use crate::model::{ChatItem, ToolCard, ViewMode, tool_icon};
use crate::session::{
    Activity, LIVE_CMD_PREVIEW_LINES, LIVE_PARALLEL_MAX, LIVE_THINK_LINES, LivePreviewContent,
    Session, TurnSummary, TurnUsage,
};
use crate::theme::rgb;
use crate::theme::*;
use crate::usage;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::*;
use gpui_kit::*;
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

impl ChatApp {
    pub(super) fn render_turn_status(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let s = self.session();
        if !s.busy {
            // 回合刚结束、底部还挂着驻留：只画那一屏，不再画转圈和 Esc
            if s.has_live_linger() {
                return self.render_live_preview(window, cx);
            }
            // 正在后台建 worktree（大仓库检出要几秒）：说一声，建好之前发的话先排队
            if s.worktree_busy.is_some() {
                return Some(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1p5()
                        .min_w_0()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .child(mascot(Mood::Scan))
                        .child(
                            shimmer_text(
                                "正在准备 worktree（现在发的话会等它建好再发）…",
                                INK_SOFT,
                                INK,
                            )
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap(),
                        ),
                );
            }
            // 续接历史会话（内核在重建上下文，好了才把消息发出去）：只在这里说一声，接上了就消失
            return s.resuming.then(|| {
                div()
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    .min_w_0()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(mascot(Mood::Wake))
                    .child(
                        shimmer_text("正在续接会话…", INK_SOFT, INK)
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap(),
                    )
            });
        }
        let activity = s.activity();
        // 左边是统一的几何状态指示；等授权要用户处理，文字是红的；进行中的活动带扫光。
        let (mood, text, color): (Mood, SharedString, Option<Color>) = match activity {
            Activity::WaitingPermission => (Mood::Ask, "等待你授权".into(), Some(RED)),
            Activity::WaitingQuestion => (Mood::Ask, "等你的回答".into(), Some(CORAL_DEEP)),
            // 限流 / 5xx：内核自己会重发，告诉用户在等什么、还要等多久（不然看着像卡死）
            Activity::Retrying => (
                Mood::Sweat,
                s.retry.as_ref().map_or_else(SharedString::default, |r| {
                    retry_label(r.status, r.attempt, r.at.saturating_duration_since(Instant::now()))
                        .into()
                }),
                None,
            ),
            Activity::Compacting => (Mood::Squish, "正在压缩上下文".into(), None),
            Activity::Replying => (Mood::Talk, "正在回复".into(), None),
            // 说清楚此刻在做什么，而不是每轮换一个随机动词
            Activity::Working => (Mood::Think, working_label(s).into_owned().into(), None),
        };
        let label = if activity_shimmers(activity) {
            shimmer_text(text, INK_SOFT, INK)
        } else if let Some(color) = color {
            div().text_color(rgb(color)).child(text)
        } else {
            div().child(text)
        };
        let mut meta: Vec<(String, Option<String>, Color)> = s
            .turn_started
            .map(|t| (elapsed_label(t.elapsed()), None, INK_SOFT))
            .into_iter()
            .collect();
        // 本轮正在下发的 token：端点已经报了的 + 还在流式的估算，一直在涨。
        // 是估的还是数出来的，看符号和深浅（见 live_usage_label）
        meta.extend(live_usage_label(s.live_output_tokens(), &s.turn_usage));
        // 顺带告诉用户怎么停（Esc 见 keys.rs）
        meta.push(("Esc 停止".into(), None, INK_SOFT));

        // 过程行里已经有一个转圈时，状态行不再叠第二个（两处一起转看着像卡了）
        let quiet = self.settings.view_mode != ViewMode::Summary
            && matches!(mood, Mood::Think | Mood::Talk)
            && s.has_visible_running_spinner();
        let indicator = if quiet {
            div()
                .size_5()
                .flex_shrink_0()
                .flex()
                .justify_center()
                .items_center()
                .child(Icon::new(IconAll::CircleDot).size_4().text_color(rgb(CORAL)))
        } else {
            mascot(mood)
        };

        // 分隔点单独一格、两侧同宽
        let mut row = div()
            .h_flex()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            // 用时每秒在变、token 流式在涨：等宽数字，后面的「Esc 停止」不跟着抖
            .font_features(tabular_nums())
            .child(indicator)
            .child(label.min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap());
        for (i, (text, tip, color)) in meta.into_iter().enumerate() {
            row = row.child(div().text_color(rgb(INK_FAINT)).child("·"));
            let cell = div().flex_shrink_0().text_color(rgb(color));
            row = match tip {
                Some(tip) => row.child(
                    cell.id(("status-meta", i))
                        .tooltip(move |window, cx| super::chat::text_tip(&tip, window, cx))
                        .child(text),
                ),
                None => row.child(cell.child(text)),
            };
        }
        // 状态行和它的实时预览槽同宽左对齐：预览卡片缩进到状态文字起点
        let mut col = div().v_flex().gap_2().min_w_0().child(row);
        if let Some(preview) = self.render_live_preview(window, cx) {
            col = col.child(preview);
        }
        Some(col)
    }

    /// 状态行下方的实时预览槽：思考 / 单个非简单工具 / 并行工具。
    /// 简单工具、子任务、等待中的状态都不出预览（见 [`Session::live_preview`]）。
    fn render_live_preview(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let s = self.session();
        match s.live_preview_content(self.settings.view_mode) {
            LivePreviewContent::None => None,
            // 思考：无框、低对比度灰、斜体，缩进到状态文字起点（不加卡片底色和描边）
            LivePreviewContent::Thinking { lines, has_more } => {
                if lines.is_empty() {
                    return None;
                }
                let mut col = div()
                    .v_flex()
                    .gap_1()
                    .min_w_0()
                    .pl(rems(1.625))
                    .pt_0p5()
                    .text_size(TEXT_SMALL)
                    .line_height(LEADING)
                    .text_color(with_alpha(rgb(INK_SOFT), 0.72))
                    .italic();
                for (n, line) in lines.iter().take(LIVE_THINK_LINES).enumerate() {
                    let mut text = line.clone();
                    if has_more && n + 1 == lines.len().min(LIVE_THINK_LINES) {
                        text = format!("{text} …").into();
                    }
                    col = col.child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(text),
                    );
                }
                Some(col)
            }
            LivePreviewContent::Tool(ix) => {
                let Some(ChatItem::Tool(card)) = s.transcript.get(ix) else {
                    return None;
                };
                Some(self.live_card(self.live_tool_body(ix, card, window, cx)))
            }
            LivePreviewContent::ParallelTools(indices) => {
                let mut col = div().v_flex().gap_1().min_w_0();
                for (n, &ix) in indices.iter().take(LIVE_PARALLEL_MAX).enumerate() {
                    let Some(ChatItem::Tool(card)) = s.transcript.get(ix) else {
                        continue;
                    };
                    let mut row = div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(
                            Icon::new(tool_icon(card.kind_key()))
                                .size_3p5()
                                .text_color(rgb(INK_SOFT)),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(rgb(INK))
                                .font_weight(FontWeight::MEDIUM)
                                .child(card.name.clone()),
                        );
                    if !card.arg.trim().is_empty() {
                        row = row.child(
                            super::chat::ellipsis_plain(
                                ("live-par-arg", n),
                                super::chat::first_line(&card.arg),
                            )
                            .font_family(MONO)
                            .text_color(rgb(INK_SOFT)),
                        );
                    }
                    col = col.child(row);
                }
                if indices.len() > LIVE_PARALLEL_MAX {
                    col = col.child(div().text_color(rgb(INK_SOFT)).child(format!(
                        "… 另有 {} 个工具运行中",
                        indices.len() - LIVE_PARALLEL_MAX
                    )));
                }
                Some(self.live_card(col))
            }
        }
    }

    /// 预览槽的卡片底：和工具展开块同样的描边 / 底色，左缩进到状态文字起点。
    fn live_card(&self, body: Div) -> Div {
        div().pl(rems(1.625)).child(
            div()
                .v_flex()
                .gap_1p5()
                .min_w_0()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(CARD))
                .text_size(TEXT_SMALL)
                .line_height(LEADING)
                .text_color(rgb(INK_SOFT))
                .child(body),
        )
    }

    /// 单个运行中工具的预览：命令或「图标 + 名称 + 参数」，下面跟输出（空时 `No output`）。
    fn live_tool_body(
        &self,
        ix: usize,
        card: &ToolCard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut col = div().v_flex().gap_1p5().min_w_0();
        if card.shows_command() {
            let lines: Vec<&str> = card.arg.trim().lines().collect();
            let shown = lines.len().min(LIVE_CMD_PREVIEW_LINES);
            let mut head =
                div().v_flex().gap_1().min_w_0().font_family(MONO).text_color(rgb(INK)).children(
                    lines[..shown].iter().enumerate().map(|(n, l)| {
                        let text = if n == 0 { format!("$ {l}") } else { (*l).to_string() };
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(text)
                    }),
                );
            if lines.len() > shown {
                head = head.child(
                    div()
                        .text_color(rgb(INK_SOFT))
                        .child(format!("… 其余 {} 行未显示", lines.len() - shown)),
                );
            }
            col = col.child(head);
        } else {
            // 工具名用正文字体、参数用等宽；输出另起一段竖排，不和这一行并排
            let mut row = div()
                .h_flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(Icon::new(tool_icon(card.kind_key())).size_3p5().text_color(rgb(INK_SOFT)))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(rgb(INK))
                        .font_weight(FontWeight::MEDIUM)
                        .child(card.name.clone()),
                );
            if !card.arg.trim().is_empty() {
                row = row.child(
                    super::chat::ellipsis_plain(
                        ("live-tool-arg", ix),
                        super::chat::first_line(&card.arg),
                    )
                    .font_family(MONO)
                    .text_color(rgb(INK_SOFT)),
                );
            }
            col = col.child(row);
        }

        let output = if card.content.trim().is_empty() {
            card.output.trim_matches('\n')
        } else {
            card.content.trim_matches('\n')
        };
        if output.is_empty() {
            return col.child(div().font_family(MONO).text_color(rgb(INK_SOFT)).child("No output"));
        }

        // 跟着输出的最新几行走：内容变了就贴底，用户手动上滚后暂停（同工具展开块的滚动方式）
        let key = ElementId::Name(format!("live-tool-out-{}-{}", self.session().token, ix).into());
        let state = window.use_keyed_state(key.clone(), cx, |_, _| LiveToolScroll {
            handle: ScrollHandle::new(),
            last_sig: None,
            follow_bottom: true,
        });
        let sig = output_sig(output);
        state.update(cx, |st, _| {
            let max = st.handle.max_offset().y.max(px(0.));
            // offset().y 顶部为 0、底部为 -max；换算成距顶距离再判断是否还贴着底
            let top = -st.handle.offset().y;
            if st.last_sig.is_some() && max > px(0.) {
                st.follow_bottom = top >= max - px(6.);
            }
            if st.last_sig != Some(sig) {
                if st.follow_bottom {
                    st.handle.scroll_to_bottom();
                }
                st.last_sig = Some(sig);
            }
        });
        let scroll = state.read(cx).handle.clone();
        let regions = self.tool_scroll_regions.clone();
        let paint_handle = scroll.clone();
        let scroll_id = key;
        col.child(
            div().border_t_1().border_color(rgb(LINE)).pt_2().child(
                div()
                    .relative()
                    .child(
                        div()
                            .id(scroll_id)
                            .font_family(MONO)
                            .max_h(rems(10.))
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .pr_4()
                            .child(div().v_flex().children(output.lines().map(|l| {
                                div().child(if l.is_empty() {
                                    " ".to_string()
                                } else {
                                    l.to_string()
                                })
                            }))),
                    )
                    .child(
                        canvas(
                            move |bounds, window, _| {
                                let visible = bounds.intersect(&window.content_mask().bounds);
                                regions.borrow_mut().push((visible, paint_handle.clone()))
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .child(Scrollbar::vertical(&scroll).mode(ScrollbarMode::Always)),
            ),
        )
    }
}

/// 预览槽里单个工具的滚动状态：贴底跟随 + 内容签名。
struct LiveToolScroll {
    handle: ScrollHandle,
    last_sig: Option<u64>,
    follow_bottom: bool,
}

fn output_sig(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// 进行中的状态文案带扫光；等用户操作或错误重试保持静态文字。
const fn activity_shimmers(activity: Activity) -> bool {
    matches!(activity, Activity::Compacting | Activity::Replying | Activity::Working)
}

/// 进程中状态的文字：并行 / 单个工具 / 正在思考（都没有就等模型）。
fn working_label(s: &Session) -> Cow<'static, str> {
    let running = s.running_tools_in_turn();
    if running.len() >= 2 {
        return format!("正在并行执行 {} 个工具", running.len()).into();
    }
    let kind = running.first().and_then(|&ix| match s.transcript.get(ix) {
        Some(ChatItem::Tool(c)) => Some(c.kind_key()),
        _ => None,
    });
    match kind {
        Some("execute") => "正在运行命令".into(),
        Some("read") => "正在读取文件".into(),
        Some("search") => "正在搜索".into(),
        Some("websearch") => "正在联网搜索".into(),
        Some("edit" | "delete" | "move") => "正在修改文件".into(),
        Some("fetch") => "正在访问网页".into(),
        Some("task") => "正在执行子任务".into(),
        Some(_) => "正在处理".into(),
        None if s.is_streaming_thinking() => "正在思考".into(),
        None => "正在处理".into(),
    }
}

/// 自动重试时状态行的文字：「网关限流，3 秒后第 1 次重试」，倒计时走完是「正在第 1 次重试」。
fn retry_label(status: u16, attempt: usize, remaining: Duration) -> String {
    let cause = if status == 429 {
        "网关限流".to_string()
    } else {
        format!("网关出错（HTTP {status}）")
    };
    match remaining.as_millis().div_ceil(1000) {
        0 => format!("{cause}，正在第 {attempt} 次重试"),
        secs => format!("{cause}，{secs} 秒后第 {attempt} 次重试"),
    }
}

/// 一轮结束后挂在回复末尾的「用时 1 分 05 秒 · ↑ 1.2k ↓ 340 token」；什么都没记到时为 None。
pub(super) fn turn_meta(t: &TurnSummary) -> Option<String> {
    let parts: Vec<String> = t
        .elapsed_ms
        .map(|ms| format!("用时 {}", elapsed_label(Duration::from_millis(ms))))
        .into_iter()
        .chain(usage_label(&t.usage))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// `23 秒` / `1 分 05 秒` / `1 小时 02 分`
fn elapsed_label(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..60 => format!("{secs} 秒"),
        60..3600 => format!("{} 分 {:02} 秒", secs / 60, secs % 60),
        _ => format!("{} 小时 {:02} 分", secs / 3600, secs % 3600 / 60),
    }
}

/// 运行状态行上的实时用量，两档深浅：
/// - `↓ 1.2k token`：端点报过用量（[`TurnUsage::reported`]），数出来的准数，用实色 `INK`；
/// - `≈ 1.2k token`：端点还一次都没报（多数 ACP agent 要等一轮结束才给），靠收到的字数估，
///   用和旁边的用时同样的 `INK_SOFT`——看着就知道还没准。
///
/// 思考量不挤在主文案里（只放悬停提示），一次都还没收到时为 None。
fn live_usage_label(output: u64, u: &TurnUsage) -> Option<(String, Option<String>, Color)> {
    if output == 0 {
        return None;
    }
    let n = usage::compact(output);
    let text = if u.reported { format!("↓ {n} token") } else { format!("≈ {n} token") };
    let tip = (u.reasoning > 0).then(|| format!("含思考 {} token", usage::compact(u.reasoning)));
    Some((text, tip, if u.reported { INK } else { INK_SOFT }))
}

/// `↑ 1.2k ↓ 340 token`；内核没报用量（演示回放、部分 agent）时不显示。
fn usage_label(u: &TurnUsage) -> Option<String> {
    if u.input == 0 && u.output == 0 {
        return None;
    }
    Some(format!("↑ {} ↓ {} token", usage::compact(u.input), usage::compact(u.output)))
}

/// 回复末尾那行用量的悬停提示：把思考和输入 / 输出的明细放进去，不给主行加括号。
pub(super) fn turn_meta_tooltip(u: &TurnUsage) -> Option<String> {
    (u.reasoning > 0).then(|| {
        format!(
            "输入 {} · 输出 {} · 含思考 {} token",
            usage::compact(u.input),
            usage::compact(u.output),
            usage::compact(u.reasoning)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use claudesktop_core::KernelEvent;

    #[core::prelude::v1::test]
    fn retry_label_counts_down() {
        assert_eq!(retry_label(429, 1, Duration::from_millis(2100)), "网关限流，3 秒后第 1 次重试");
        assert_eq!(retry_label(502, 2, Duration::ZERO), "网关出错（HTTP 502），正在第 2 次重试");
    }

    #[core::prelude::v1::test]
    fn working_label_names_the_running_tool() {
        fn session_with_run(kind: &str) -> Session {
            let mut s = Session::new(
                1,
                crate::settings::KernelChoice::Acp("custom-1".into()),
                std::path::PathBuf::from("."),
            );
            s.apply_event(
                KernelEvent::ToolCallStarted {
                    id: "a".into(),
                    title: "t".into(),
                    status: "in_progress".into(),
                    kind: kind.into(),
                    parent: None,
                },
                None,
            );
            s
        }
        assert_eq!(working_label(&session_with_run("execute")), "正在运行命令");
        assert_eq!(working_label(&session_with_run("read")), "正在读取文件");
        assert_eq!(working_label(&session_with_run("edit")), "正在修改文件");
        assert_eq!(working_label(&session_with_run("task")), "正在执行子任务");
        // 两个工具并行：先说有几个在跑
        let mut par = session_with_run("execute");
        par.apply_event(
            KernelEvent::ToolCallStarted {
                id: "b".into(),
                title: "t".into(),
                status: "in_progress".into(),
                kind: "fetch".into(),
                parent: None,
            },
            None,
        );
        assert_eq!(working_label(&par), "正在并行执行 2 个工具");
        // 没有工具、但还在流式思考
        let mut thinking = Session::new(
            1,
            crate::settings::KernelChoice::Acp("custom-1".into()),
            std::path::PathBuf::from("."),
        );
        thinking.transcript.push(ChatItem::Thinking {
            text: "先想想".into(),
            streaming: true,
            open: false,
            started: None,
            elapsed_ms: None,
        });
        assert_eq!(working_label(&thinking), "正在思考");
        // 没有在跑的工具（等模型）：说「正在处理」
        let idle = Session::new(
            1,
            crate::settings::KernelChoice::Acp("custom-1".into()),
            std::path::PathBuf::from("."),
        );
        assert_eq!(working_label(&idle), "正在处理");
    }

    #[core::prelude::v1::test]
    fn elapsed_label_scales_units() {
        assert_eq!(elapsed_label(Duration::from_secs(23)), "23 秒");
        assert_eq!(elapsed_label(Duration::from_secs(65)), "1 分 05 秒");
        assert_eq!(elapsed_label(Duration::from_secs(3720)), "1 小时 02 分");
    }

    #[core::prelude::v1::test]
    fn usage_label_hides_empty_and_keeps_the_main_line_clean() {
        assert_eq!(usage_label(&TurnUsage::default()), None);
        let u = TurnUsage { input: 1200, output: 340, reasoning: 120, reported: true };
        assert_eq!(usage_label(&u).unwrap(), "↑ 1,200 ↓ 340 token");
        assert_eq!(turn_meta_tooltip(&u).unwrap(), "输入 1,200 · 输出 340 · 含思考 120 token");
        let u = TurnUsage { input: 5, output: 1, reasoning: 0, reported: true };
        assert_eq!(usage_label(&u).unwrap(), "↑ 5 ↓ 1 token");
        assert_eq!(turn_meta_tooltip(&u), None);
    }

    #[core::prelude::v1::test]
    fn live_usage_label_counts_what_is_being_streamed() {
        // 一次都还没收到：不显示
        assert_eq!(live_usage_label(0, &TurnUsage::default()), None);
        // 端点还没报过用量：全靠流式估算，是「≈」+ 次要色
        assert_eq!(
            live_usage_label(15, &TurnUsage::default()),
            Some(("≈ 15 token".into(), None, INK_SOFT))
        );
        // 端点报过：准数「↓」+ 实色，之后又流了一点就接着加；思考明细放悬停提示
        let reported = TurnUsage { input: 1200, output: 340, reasoning: 120, reported: true };
        assert_eq!(
            live_usage_label(340, &reported),
            Some(("↓ 340 token".into(), Some("含思考 120 token".into()), INK))
        );
        assert_eq!(
            live_usage_label(420, &reported),
            Some(("↓ 420 token".into(), Some("含思考 120 token".into()), INK))
        );
        // 只认输出，输入不算进去
        let only_input = TurnUsage { input: 5000, output: 0, reasoning: 0, reported: true };
        assert_eq!(live_usage_label(0, &only_input), None);
    }

    #[core::prelude::v1::test]
    fn streamed_estimate_follows_ascii_and_wide_chars() {
        let mut s = crate::usage::Streamed::default();
        assert_eq!(s.tokens(), 0);
        // 英文 4 字节约 1 token，零头留着（不每块取整丢成 0）
        s.push("abc");
        assert_eq!(s.tokens(), 0);
        s.push("defgh");
        assert_eq!(s.tokens(), 2);
        // 中文按 1 字 1 token
        s.push("你好");
        assert_eq!(s.tokens(), 4);
        // 端点报过之后重新估
        s.clear();
        assert_eq!(s.tokens(), 0);
    }

    #[core::prelude::v1::test]
    fn turn_meta_joins_what_is_known() {
        let usage = TurnUsage { input: 5, output: 1, reasoning: 0, reported: true };
        let t = TurnSummary { elapsed_ms: Some(65_000), usage };
        assert_eq!(turn_meta(&t).unwrap(), "用时 1 分 05 秒 · ↑ 5 ↓ 1 token");
        // 旧日志没有用时；演示回放没有用量
        assert_eq!(turn_meta(&TurnSummary { elapsed_ms: None, usage }).unwrap(), "↑ 5 ↓ 1 token");
        let t = TurnSummary { elapsed_ms: Some(3_000), usage: TurnUsage::default() };
        assert_eq!(turn_meta(&t).unwrap(), "用时 3 秒");
        let t = TurnSummary { elapsed_ms: None, usage: TurnUsage::default() };
        assert_eq!(turn_meta(&t), None);
    }

    #[core::prelude::v1::test]
    fn activity_shimmers_only_for_active_progress() {
        assert!(activity_shimmers(Activity::Working));
        assert!(activity_shimmers(Activity::Replying));
        assert!(activity_shimmers(Activity::Compacting));
        assert!(!activity_shimmers(Activity::WaitingPermission));
        assert!(!activity_shimmers(Activity::WaitingQuestion));
        assert!(!activity_shimmers(Activity::Retrying));
    }
}
