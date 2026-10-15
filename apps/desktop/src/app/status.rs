//! 对话流末尾的运行状态行：本轮在干什么、跑了多久、用了多少 token、怎么停
//! （对照官方「✳ 5s · ↓ 3.5k tokens · esc to interrupt」）。只在当前会话忙时出现；
//! 数据来自 `Session::activity` / `turn_usage` / `turn_started`。
//! 一轮结束后用时和用量挂到回复末尾（[`turn_meta`]）。

use super::ChatApp;
use crate::session::{Activity, TurnSummary, TurnUsage};
use crate::theme::*;
use crate::theme::rgb;
use crate::usage;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::*;
use gpui_kit::*;
use std::time::{Duration, Instant};

impl ChatApp {
    pub(super) fn render_turn_status(&self) -> Option<Div> {
        let s = self.session();
        if !s.busy {
            return None;
        }
        let activity = s.activity();
        // 等授权要用户处理：静止红点、红字；其余转圈。
        // 只分三档：具体在跑哪一步看上面的过程组，这里不跟着每个工具来回切
        let (indicator, label) = match activity {
            Activity::WaitingPermission => (
                div().size(px(6.)).rounded_full().bg(rgb(RED)).into_any_element(),
                div().text_color(rgb(RED)).child("等待你授权"),
            ),
            // 限流 / 5xx：内核自己会重发，告诉用户在等什么、还要等多久（不然看着像卡死）
            Activity::Retrying => (
                Spinner::new().xsmall().color(rgb(CORAL).into()).into_any_element(),
                div().child(s.retry.as_ref().map_or_else(String::new, |r| {
                    retry_label(r.status, r.attempt, r.at.saturating_duration_since(Instant::now()))
                })),
            ),
            Activity::Compacting => (
                Spinner::new().xsmall().color(rgb(CORAL).into()).into_any_element(),
                div().child("正在压缩上下文"),
            ),
            Activity::Working | Activity::Replying => (
                Spinner::new().xsmall().color(rgb(CORAL).into()).into_any_element(),
                div().child(if activity == Activity::Replying { "正在回复" } else { "工作中" }),
            ),
        };
        let mut meta: Vec<String> = s.turn_started.map(|t| elapsed_label(t.elapsed())).into_iter().collect();
        meta.extend(usage_label(&s.turn_usage));
        // 顺带告诉用户怎么停（Esc 见 keys.rs）
        meta.push("Esc 停止".into());

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
            .child(
                div()
                    .size(px(16.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .items_center()
                    .mr_0p5()
                    .child(indicator),
            )
            .child(label);
        for text in meta {
            row = row
                .child(div().text_color(rgb(INK_FAINT)).child("·"))
                .child(div().flex_shrink_0().child(text));
        }
        Some(row)
    }
}

/// 自动重试时状态行的文字：「网关限流，3 秒后第 1 次重试」，倒计时走完是「正在第 1 次重试」。
fn retry_label(status: u16, attempt: usize, remaining: Duration) -> String {
    let cause = if status == 429 { "网关限流".to_string() } else { format!("网关出错（HTTP {status}）") };
    match remaining.as_millis().div_ceil(1000) {
        0 => format!("{cause}，正在第 {attempt} 次重试"),
        secs => format!("{cause}，{secs} 秒后第 {attempt} 次重试"),
    }
}

/// 一轮结束后挂在回复末尾的「用时 1m 05s · ↑ 1.2k ↓ 340 token」；什么都没记到时为 None。
pub(super) fn turn_meta(t: &TurnSummary) -> Option<String> {
    let parts: Vec<String> = t
        .elapsed_ms
        .map(|ms| format!("用时 {}", elapsed_label(Duration::from_millis(ms))))
        .into_iter()
        .chain(usage_label(&t.usage))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// `23s` / `1m 05s` / `1h 02m`
fn elapsed_label(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

/// `↑ 1.2k ↓ 340 token（思考 120）`；内核没报用量（演示回放、部分 agent）时不显示。
fn usage_label(u: &TurnUsage) -> Option<String> {
    if u.input == 0 && u.output == 0 {
        return None;
    }
    let mut text = format!(
        "↑ {} ↓ {} token",
        usage::compact(u.input),
        usage::compact(u.output)
    );
    if u.reasoning > 0 {
        text += &format!("（思考 {}）", usage::compact(u.reasoning));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn retry_label_counts_down() {
        assert_eq!(retry_label(429, 1, Duration::from_millis(2100)), "网关限流，3 秒后第 1 次重试");
        assert_eq!(retry_label(502, 2, Duration::ZERO), "网关出错（HTTP 502），正在第 2 次重试");
    }

    #[core::prelude::v1::test]
    fn elapsed_label_scales_units() {
        assert_eq!(elapsed_label(Duration::from_secs(23)), "23s");
        assert_eq!(elapsed_label(Duration::from_secs(65)), "1m 05s");
        assert_eq!(elapsed_label(Duration::from_secs(3720)), "1h 02m");
    }

    #[core::prelude::v1::test]
    fn usage_label_hides_empty_and_shows_reasoning() {
        assert_eq!(usage_label(&TurnUsage::default()), None);
        let u = TurnUsage { input: 1200, output: 340, reasoning: 120 };
        assert_eq!(usage_label(&u).unwrap(), "↑ 1,200 ↓ 340 token（思考 120）");
        let u = TurnUsage { input: 5, output: 1, reasoning: 0 };
        assert_eq!(usage_label(&u).unwrap(), "↑ 5 ↓ 1 token");
    }

    #[core::prelude::v1::test]
    fn turn_meta_joins_what_is_known() {
        let usage = TurnUsage { input: 5, output: 1, reasoning: 0 };
        let t = TurnSummary { elapsed_ms: Some(65_000), usage };
        assert_eq!(turn_meta(&t).unwrap(), "用时 1m 05s · ↑ 5 ↓ 1 token");
        // 旧日志没有用时；演示回放没有用量
        assert_eq!(turn_meta(&TurnSummary { elapsed_ms: None, usage }).unwrap(), "↑ 5 ↓ 1 token");
        let t = TurnSummary { elapsed_ms: Some(3_000), usage: TurnUsage::default() };
        assert_eq!(turn_meta(&t).unwrap(), "用时 3s");
        let t = TurnSummary { elapsed_ms: None, usage: TurnUsage::default() };
        assert_eq!(turn_meta(&t), None);
    }
}
