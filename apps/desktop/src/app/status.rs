//! 对话流末尾的运行状态行：本轮在干什么、跑了多久、用了多少 token、怎么停
//! （对照官方「✳ 5s · ↓ 3.5k tokens · esc to interrupt」）。只在当前会话忙时出现；
//! 数据来自 `Session::activity` / `turn_usage` / `turn_started`。

use super::ChatApp;
use crate::session::{Activity, TurnUsage};
use crate::theme::*;
use crate::usage;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Duration;

impl ChatApp {
    pub(super) fn render_turn_status(&self) -> Option<Div> {
        let s = self.session();
        if !s.busy {
            return None;
        }
        let activity = s.activity();
        let waiting = activity == Activity::WaitingPermission;
        // 等授权要用户处理：静止红点；其余转圈
        let indicator = if waiting {
            div().size(px(6.)).rounded_full().bg(rgb(RED)).into_any_element()
        } else {
            Spinner::new().xsmall().color(rgb(CORAL).into()).into_any_element()
        };
        let label = match &activity {
            Activity::WaitingPermission => div().text_color(rgb(RED)).child("等待授权"),
            Activity::Tool { name, arg } => div()
                .h_flex()
                .min_w_0()
                .gap_1p5()
                .child("正在执行")
                .child(div().text_color(rgb(INK)).child(name.clone()))
                .when(!arg.is_empty(), |d| {
                    d.child(
                        div()
                            .max_w(px(320.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(MONO)
                            .child(arg.clone()),
                    )
                }),
            Activity::Thinking => div().child("思考中"),
            Activity::Replying => div().child("正在回复"),
            Activity::Waiting => div().child("等待响应"),
        };
        let mut meta: Vec<String> = s.turn_started.map(|t| elapsed_label(t.elapsed())).into_iter().collect();
        meta.extend(usage_label(&s.turn_usage));
        // 顺带告诉用户怎么停（Esc 见 keys.rs）
        meta.push("Esc 停止".into());

        Some(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child(
                    div()
                        .size(px(14.))
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .items_center()
                        .child(indicator),
                )
                .child(label)
                .children(meta.into_iter().map(|text| {
                    div()
                        .flex_shrink_0()
                        .text_color(rgb(INK_SOFT))
                        .child(format!("· {text}"))
                })),
        )
    }
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
}
