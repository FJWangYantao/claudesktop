//! 对话流末尾的运行状态行：本轮在干什么、跑了多久、正在下发多少 token、怎么停。
//! 指示器使用极简几何图标或统一圆弧，不再使用卡通表情。
//! 数据来自 `Session::activity` / `turn_usage` / `turn_started` / `live_output_tokens`
//! （后者是本轮输出量：端点报过的 + 还在流式、靠字数估的，见 `usage::Streamed`）。
//! 数出来的和估出来的得分得出来：端点报过用量是实色的 `↓ 1.2k token`，
//! 还没报过是次要色的 `≈ 1.2k token`（见 [`live_usage_label`]）。
//! 一轮结束后用时和用量挂到回复末尾（[`turn_meta`]）。

use super::ChatApp;
use super::mascot::Mood;
use super::motion::mascot;
use crate::model::{ChatItem, ToolPhase, ViewMode};
use crate::session::{Activity, Session, TurnSummary, TurnUsage};
use crate::theme::rgb;
use crate::theme::*;
use crate::usage;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;
use std::time::{Duration, Instant};

impl ChatApp {
    pub(super) fn render_turn_status(&self) -> Option<Div> {
        let s = self.session();
        if !s.busy {
            // 正在后台建 worktree（大仓库检出要几秒）：说一声，建好之前发的话先排队
            if s.worktree_busy.is_some() {
                return Some(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .child(mascot(Mood::Scan))
                        .child("正在准备 worktree（现在发的话会等它建好再发）…"),
                );
            }
            // 续接历史会话（内核在重建上下文，好了才把消息发出去）：只在这里说一声，接上了就消失
            return s.resuming.then(|| {
                div()
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(mascot(Mood::Wake))
                    .child("正在续接会话…")
            });
        }
        let activity = s.activity();
        // 左边是统一的几何状态指示；等授权要用户处理，文字是红的。
        let (mood, label) = match activity {
            Activity::WaitingPermission => {
                (Mood::Ask, div().text_color(rgb(RED)).child("等待你授权"))
            }
            Activity::WaitingQuestion => {
                (Mood::Ask, div().text_color(rgb(CORAL_DEEP)).child("等你的回答"))
            }
            // 限流 / 5xx：内核自己会重发，告诉用户在等什么、还要等多久（不然看着像卡死）
            Activity::Retrying => (
                Mood::Sweat,
                div().child(s.retry.as_ref().map_or_else(String::new, |r| {
                    retry_label(r.status, r.attempt, r.at.saturating_duration_since(Instant::now()))
                })),
            ),
            Activity::Compacting => (Mood::Squish, div().child("正在压缩上下文")),
            Activity::Replying => (Mood::Talk, div().child("正在回复")),
            // 说清楚此刻在做什么，而不是每轮换一个随机动词
            Activity::Working => (Mood::Think, div().child(working_label(s))),
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
        Some(row)
    }
}

/// 进程中状态的文字：看正在跑的工具（没有就等模型）。
fn working_label(s: &Session) -> &'static str {
    let kind = s.transcript.iter().rev().find_map(|i| match i {
        ChatItem::Tool(c) if c.phase == ToolPhase::Running => Some(c.kind_key()),
        _ => None,
    });
    match kind {
        Some("execute") => "正在运行命令",
        Some("read") => "正在读取文件",
        Some("search") => "正在搜索",
        Some("edit" | "delete" | "move") => "正在修改文件",
        Some("fetch") => "正在访问网页",
        Some("task") => "正在执行子任务",
        _ => "正在处理",
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
}
