//! 输入框下方的上下文用量：一枚小圆环 + 百分比（平时就在），悬停展开一张用量卡。
//!
//! 圆环是矢量画的（同 `motion` 的转圈）：淡色轨道 + 一段圆头圆弧，起点在正上方顺时针。
//! 四分之三以下保持次要色、不打扰；过七成转赤陶，过九成连百分比一起变红
//! （再发很可能超出上限，`composer.rs` 那边还会顶一行提醒）。

use super::ChatApp;
use super::motion::{on_circle, paint_dot};
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use crate::usage::{self, ContextGauge};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 圆环直径（rem：和旁边 14px 的图标同档，跟界面字号一起缩放）与描边占直径的比例
/// （同 `motion::spinner`：14px 下约 2px）
const RING_SIZE: Rems = rems(0.875);
const RING_STROKE_RATIO: f32 = 0.15;
/// 悬停卡宽度（rem：跟着界面字号缩放，否则放大字号后数字挤得折行）
const CARD_W: Rems = rems(17.);

impl ChatApp {
    /// 输入框下方的上下文用量；没报告过用量时为 None（只有自研内核报告，窗口大小也只有它知道）。
    pub(super) fn context_gauge(&self) -> Option<ContextGauge> {
        // TEMP-EYEBALL: 截图用，稍后删掉
        if let Ok(v) = std::env::var("CD_EYEBALL_GAUGE") {
            let (used, window) = v.split_once('/').unwrap_or((&v, "256000"));
            return Some(usage::context_gauge(
                used.parse().unwrap_or(23_400),
                window.parse().ok(),
            ));
        }
        let s = self.session();
        if s.context_tokens == 0 {
            return None;
        }
        let window = match s.kernel {
            KernelChoice::Builtin => self.settings.builtin.context_window(&s.model_label),
            _ => None,
        };
        Some(usage::context_gauge(s.context_tokens, window))
    }

    /// 圆环 + 百分比，悬停看用量卡。
    pub(super) fn render_context_gauge(&self) -> Option<Div> {
        let gauge = self.context_gauge()?;
        let tone = tone(gauge.percent);
        // 数字和圆环同一个色档：过七成赤陶、过九成红，一眼看出该管管了
        let text = match gauge.percent {
            Some(p) if p >= 90 => RED,
            Some(p) if p >= 70 => CORAL_DEEP,
            _ => INK_SOFT,
        };
        let cache_hit = self.session().cache_hit;
        let card_gauge = gauge.clone();
        let trigger = div()
            .h_flex()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_default()
            .text_size(TEXT_SMALL)
            .text_color(rgb(text))
            .font_features(tabular_nums())
            .hover(|d| d.bg(rgb(HOVER)))
            .when_some(gauge.percent, |d, p| d.child(Self::context_ring(p, tone)))
            .child(gauge.label.clone());
        Some(
            div().child(
                HoverCard::new("context-gauge")
                    .anchor(Anchor::BottomRight)
                    .open_delay(std::time::Duration::from_millis(220))
                    .close_delay(std::time::Duration::from_millis(160))
                    .trigger(trigger)
                    .content(move |_, _, _| Self::context_card(card_gauge.clone(), cache_hit)),
            ),
        )
    }

    /// 悬停展开的用量卡：标题 + 进度条 + 具体数字，底部一句「到八成会怎样」。
    fn context_card(gauge: ContextGauge, cache_hit: Option<u32>) -> Div {
        let tone = tone(gauge.percent);
        let mut rows = div().v_flex().gap_1p5().w_full();
        if let Some(window) = gauge.window {
            rows = rows.child(row(
                "已用",
                format!(
                    "{} / {} token",
                    usage::short_tokens(gauge.used),
                    usage::short_tokens(window)
                ),
            ));
        } else {
            rows = rows.child(row("已用", format!("{} token", usage::short_tokens(gauge.used))));
        }
        if let Some(left) = gauge.until_compact {
            rows =
                rows.child(row("到自动压缩还剩", format!("{} token", usage::short_tokens(left))));
        }
        if let Some(hit) = cache_hit {
            rows = rows.child(row("上一轮命中缓存", format!("{hit}%")));
        }
        let caption = if gauge.window.is_some() {
            format!(
                "到 {}% 时，下一条消息发出前会先把较早的对话压缩成摘要",
                usage::AUTO_COMPACT_PERCENT
            )
        } else {
            "端点没报告窗口大小，只能看到用了多少".to_string()
        };

        div()
            .v_flex()
            .w(CARD_W)
            .gap_2p5()
            .child(
                div()
                    .h_flex()
                    .items_baseline()
                    .justify_between()
                    .gap_2()
                    .child(
                        div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child("上下文用量"),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(TEXT_LEAD)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .font_features(tabular_nums())
                            .child(gauge.label.clone()),
                    ),
            )
            .when_some(gauge.percent, |d, p| {
                d.child(Progress::new("context-bar").value(p as f32).color(rgb(tone)).small())
            })
            .child(rows)
            .child(
                div()
                    .text_size(TEXT_META)
                    .line_height(LEADING)
                    .text_color(rgb(INK_SOFT))
                    .child(caption),
            )
    }

    /// 小圆环：淡色轨道 + 一段圆头圆弧，矢量画（同 `motion::spinner` 的做法）。
    fn context_ring(percent: u32, tone: Color) -> Div {
        let color = rgb(tone);
        let sweep = (percent as f32 / 100.).clamp(0., 1.);
        div().flex_shrink_0().size(RING_SIZE).child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let d = bounds.size.width.min(bounds.size.height);
                    let stroke = d * RING_STROKE_RATIO;
                    let r = (d - stroke) / 2.;
                    let c = bounds.center();
                    // 轨道：同色的淡圆环，用得还少的时候也看得出是个圈
                    let mut ring = PathBuilder::stroke(stroke);
                    ring.move_to(on_circle(c, r, 0.));
                    ring.arc_to(point(r, r), px(0.), false, true, on_circle(c, r, 0.5));
                    ring.arc_to(point(r, r), px(0.), false, true, on_circle(c, r, 0.));
                    if let Ok(path) = ring.build() {
                        window.paint_path(path, Rgba { a: 0.18, ..color });
                    }
                    if sweep <= 0.002 {
                        return;
                    }
                    // 满了就是整个圈：弧的两端重合，画出来只有一个圆头
                    let (from, to) = match sweep {
                        s if s >= 0.998 => (on_circle(c, r, 0.), on_circle(c, r, 0.999)),
                        s => (on_circle(c, r, 0.), on_circle(c, r, s)),
                    };
                    let mut arc = PathBuilder::stroke(stroke);
                    arc.move_to(from);
                    arc.arc_to(point(r, r), px(0.), sweep > 0.5, true, to);
                    if let Ok(path) = arc.build() {
                        window.paint_path(path, color);
                    }
                    if sweep < 0.998 {
                        // 描边是平头：两端各补一个半个线宽的圆点当圆头
                        paint_dot(from, stroke / 2., color, window);
                        paint_dot(to, stroke / 2., color, window);
                    }
                },
            )
            .size_full(),
        )
    }
}

/// 用量对应的警示色：平时次要色，过七成赤陶，过九成红。
fn tone(percent: Option<u32>) -> Color {
    match percent {
        Some(p) if p >= 90 => RED,
        Some(p) if p >= 70 => CORAL,
        _ => INK_SOFT,
    }
}

/// 卡里的一行：左边说明、右边数字（等宽，换行时数字不会跟着抖）。
fn row(label: &'static str, value: String) -> Div {
    div()
        .h_flex()
        .items_baseline()
        .justify_between()
        .gap_2()
        .text_size(TEXT_SMALL)
        .child(div().flex_shrink_0().text_color(rgb(INK_SOFT)).child(label))
        .child(div().text_right().text_color(rgb(INK)).font_features(tabular_nums()).child(value))
}

/// 圆环的圆弧长度（0..1），单测用。
#[cfg(test)]
fn sweep_of(percent: u32) -> f32 {
    (percent as f32 / 100.).clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn tone_turns_at_seventy_and_ninety() {
        assert_eq!(tone(Some(9)), INK_SOFT);
        assert_eq!(tone(Some(70)), CORAL);
        assert_eq!(tone(Some(89)), CORAL);
        assert_eq!(tone(Some(90)), RED);
        assert_eq!(tone(None), INK_SOFT);
    }

    #[core::prelude::v1::test]
    fn sweep_is_a_fraction_of_the_ring() {
        assert_eq!(sweep_of(0), 0.);
        assert_eq!(sweep_of(50), 0.5);
        assert_eq!(sweep_of(100), 1.);
    }
}
