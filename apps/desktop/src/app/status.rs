//! 输入框上方的运行状态条：本轮在干什么、跑了多久、用了多少 token。
//! 只在当前会话忙时出现；数据来自 `Session::activity` / `turn_usage` / `turn_started`。

use super::ChatApp;
use crate::session::{Activity, TurnUsage};
use crate::theme::*;
use crate::usage;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Duration;

impl ChatApp {
    pub(super) fn render_turn_status(&self) -> Option<StatusBar> {
        let s = self.session();
        if !s.busy {
            return None;
        }
        let activity = s.activity();
        let waiting = activity == Activity::WaitingPermission;
        // 等授权要用户处理：静止红点；其余转圈
        let indicator = if waiting {
            div()
                .size(px(6.))
                .mx(px(3.))
                .rounded_full()
                .bg(rgb(RED))
                .into_any_element()
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
        let elapsed = s.turn_started.map(|t| elapsed_label(t.elapsed()));

        Some(
            StatusBar::new()
                .left(indicator)
                .left(label)
                .when_some(usage_label(&s.turn_usage), |bar, text| bar.right(text))
                .when_some(elapsed, |bar, text| {
                    bar.right(div().font_family(MONO).text_color(rgb(INK_SOFT)).child(text))
                })
                // 覆盖组件默认的底栏样式：融进纸底，不要顶部分隔线
                .bg(rgb(PAPER))
                .border_color(rgba(0x00000000))
                .px_3()
                .text_size(TEXT_META)
                .text_color(rgb(INK_SOFT)),
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

/// `输入 1,200 · 输出 340（思考 120）`；内核没报用量（演示回放、部分 agent）时不显示。
fn usage_label(u: &TurnUsage) -> Option<String> {
    if u.input == 0 && u.output == 0 {
        return None;
    }
    let mut text = format!(
        "输入 {} · 输出 {}",
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
        assert_eq!(usage_label(&u).unwrap(), "输入 1,200 · 输出 340（思考 120）");
        let u = TurnUsage { input: 5, output: 1, reasoning: 0 };
        assert_eq!(usage_label(&u).unwrap(), "输入 5 · 输出 1");
    }
}
