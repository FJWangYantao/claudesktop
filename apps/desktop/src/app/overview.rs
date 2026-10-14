//! 空会话首页：问候语 + 用量概览（数据来自会话日志，见 usage.rs）。

use super::ChatApp;
use crate::model::*;
use crate::theme::*;
use crate::usage::{self, HEAT_COLS, HEAT_ROWS, Overview};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;
use std::time::{SystemTime, UNIX_EPOCH};

impl ChatApp {
    pub(super) fn heat_color(v: u8) -> u32 {
        HEAT[(v as usize).min(HEAT.len() - 1)]
    }

    fn stat_card(c: &StatCard) -> Div {
        div()
            .v_flex()
            .gap_1()
            .w(px(220.))
            .p_3()
            .rounded_lg()
            .bg(rgb(CARD))
            .child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(c.label),
            )
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(TEXT_LEAD)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(INK))
                    .child(c.value.clone()),
            )
    }

    /// 概览页签：6 张统计卡 + 最近 26 周热力图
    fn render_usage_cards(o: &Overview) -> Div {
        let dash = || SharedString::from("—");
        let stats = [
            StatCard { label: "会话数", value: usage::compact(o.sessions as u64).into() },
            StatCard { label: "消息数", value: usage::compact(o.messages as u64).into() },
            StatCard { label: "Token 总量", value: usage::compact(o.tokens).into() },
            StatCard { label: "活跃天数", value: o.active_days.to_string().into() },
            StatCard {
                label: "高峰时段",
                value: o.peak_hour.map_or_else(dash, |h| usage::hour_label(h).into()),
            },
            StatCard {
                label: "最常用模型",
                value: o.models.first().map_or_else(dash, |m| m.name.clone().into()),
            },
        ];

        // 格子按列数均分整行宽度，与上面的统计卡两端对齐
        let mut grid = div().v_flex().w_full().gap(px(3.));
        for row in 0..HEAT_ROWS {
            let mut line = div().h_flex().w_full().gap(px(3.));
            for col in 0..HEAT_COLS {
                let cell = div().flex_1().h(px(14.)).rounded(px(3.));
                line = line.child(match o.heat[row * HEAT_COLS + col] {
                    Some(v) => cell.bg(rgb(Self::heat_color(v))),
                    None => cell,
                });
            }
            grid = grid.child(line);
        }

        div()
            .v_flex()
            .gap_4()
            .child(
                div()
                    .v_flex()
                    .gap_3()
                    .child(div().h_flex().gap_3().children(stats[..3].iter().map(Self::stat_card)))
                    .child(div().h_flex().gap_3().children(stats[3..].iter().map(Self::stat_card))),
            )
            .child(grid)
    }

    /// 模型页签：每个模型的轮数和 token，条形按轮数
    fn render_usage_models(o: &Overview) -> Div {
        let mut list = div().v_flex().gap_2().w(px(684.));
        if o.models.is_empty() {
            return list.child(
                div()
                    .py_6()
                    .text_size(TEXT_BODY)
                    .text_color(rgb(INK_SOFT))
                    .child("这段时间还没有记录到模型（ACP agent 不上报模型时不计入）"),
            );
        }
        let max = o.models.first().map_or(1, |m| m.turns.max(1));
        for m in o.models.iter().take(8) {
            let share = m.turns as f32 / max as f32;
            list = list.child(
                div()
                    .v_flex()
                    .gap_1()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(CARD))
                    .child(
                        div()
                            .h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .font_family(MONO)
                                    .text_size(TEXT_BODY)
                                    .text_color(rgb(INK))
                                    .child(m.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(TEXT_SMALL)
                                    .text_color(rgb(INK_SOFT))
                                    .child(format!(
                                        "{} 轮 · {} token",
                                        usage::compact(m.turns as u64),
                                        usage::compact(m.tokens)
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .h(px(6.))
                            .w_full()
                            .rounded_full()
                            .bg(rgb(Self::heat_color(0)))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(share))
                                    .rounded_full()
                                    .bg(rgb(Self::heat_color(3))),
                            ),
                    ),
            );
        }
        list
    }

    fn tab(&self, id: &'static str, label: &'static str, active: bool, cx: &mut Context<Self>, set: fn(&mut Self)) -> Stateful<Div> {
        div()
            .id(id)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                set(this);
                cx.notify();
            }))
            .child(Self::seg(label, active))
    }

    pub(super) fn render_overview(&self, cx: &mut Context<Self>) -> Div {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let range = match self.overview_range {
            1 => Some(30),
            2 => Some(7),
            _ => None,
        };
        let o = usage::overview(&self.usage, now, usage::local_offset_secs(), range);
        let body = if self.overview_tab == 1 {
            Self::render_usage_models(&o)
        } else {
            Self::render_usage_cards(&o)
        };

        div()
            .v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_6()
            .child(
                // 问候语
                div()
                    .h_flex()
                    .gap_2p5()
                    .items_center()
                    .child(
                        Icon::new(IconAll::Sparkles)
                            .size_6()
                            .text_color(rgb(CORAL)),
                    )
                    .child(
                        div()
                            .text_size(TEXT_TITLE)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(INK))
                            .child(greeting()),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .gap_4()
                    .w(px(724.))
                    .p_5()
                    .rounded_xl()
                    .bg(rgb(SAND))
                    .child(
                        // 顶行：左侧 tabs，右侧范围
                        div()
                            .h_flex()
                            .items_center()
                            .child(
                                div()
                                    .h_flex()
                                    .gap_1()
                                    .child(self.tab("tab-overview", "概览", self.overview_tab == 0, cx, |t| t.overview_tab = 0))
                                    .child(self.tab("tab-model", "模型", self.overview_tab == 1, cx, |t| t.overview_tab = 1)),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .h_flex()
                                    .gap_1()
                                    .child(self.tab("range-all", "全部", self.overview_range == 0, cx, |t| t.overview_range = 0))
                                    .child(self.tab("range-30", "30 天", self.overview_range == 1, cx, |t| t.overview_range = 1))
                                    .child(self.tab("range-7", "7 天", self.overview_range == 2, cx, |t| t.overview_range = 2)),
                            ),
                    )
                    .child(body)
                    .child(
                        div()
                            .pt_1()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child(usage::book_line(o.tokens)),
                    ),
            )
    }
}

/// 「下午好，WYT」：时段 + 系统用户名（取不到就不带名字）
fn greeting() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
        + usage::local_offset_secs();
    let part = match secs.rem_euclid(86_400) / 3600 {
        5..12 => "上午好",
        12..18 => "下午好",
        _ => "晚上好",
    };
    match std::env::var("USERNAME").or_else(|_| std::env::var("USER")) {
        Ok(name) if !name.trim().is_empty() => format!("{part}，{name}"),
        _ => part.to_string(),
    }
}
