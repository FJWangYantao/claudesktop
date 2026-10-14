//! 空会话首页：问候语 + 用量概览。

use super::ChatApp;
use crate::model::*;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;

impl ChatApp {
    pub(super) fn heat_color(v: u8) -> u32 {
        match v {
            0 => 0xE7E2D6,   // 空白格：沙色
            1 => 0xD8DFF0,   // 浅蓝
            2 => 0xA9B9DC,   // 中蓝
            3 => 0x7C92C4,   // 深蓝
            _ => 0x4C62A6,   // 最深
        }
    }

    pub(super) fn render_overview(&self, cx: &mut Context<Self>) -> Div {
        // 热力图假数据：6 行 × 26 列，0..=4 强度
        let mut heat: Vec<u8> = Vec::with_capacity(6 * 26);
        let mut seed: u32 = 0x9E3779B9;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 100) as u8
        };
        for i in 0..(6 * 26) {
            let r = next();
            // 稀疏分布：大多数为 0
            let v = if r < 78 {
                0
            } else if r < 88 {
                1
            } else if r < 94 {
                2
            } else if r < 98 {
                3
            } else {
                4
            };
            // 末尾几天制造最近活跃
            let v = if i >= 6 * 26 - 3 && v == 0 { (i % 3 + 2) as u8 } else { v };
            heat.push(v);
        }

        let stats = [
            StatCard { label: "会话数", value: "87".into() },
            StatCard { label: "消息数", value: "4,194".into() },
            StatCard { label: "Token 总量", value: "299.4M".into() },
            StatCard { label: "活跃天数", value: "9".into() },
            StatCard { label: "高峰时段", value: "下午 12 点".into() },
            StatCard { label: "最常用模型", value: "GLM-5.3".into() },
        ];

        let mut grid = div().v_flex().gap(px(3.));
        for row in 0..6 {
            let mut line = div().h_flex().gap(px(3.));
            for col in 0..26 {
                let v = heat[row * 26 + col];
                line = line.child(
                    div()
                        .size(px(14.))
                        .rounded(px(3.))
                        .bg(rgb(Self::heat_color(v))),
                );
            }
            grid = grid.child(line);
        }

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
                            .size_5()
                            .text_color(rgb(CORAL)),
                    )
                    .child(
                        div()
                            .font_family(SERIF)
                            .text_size(px(34.))
                            .text_color(rgb(INK))
                            .child("What's up next, WYT?"),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .gap_4()
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
                                    .child(
                                        div()
                                            .id("tab-overview")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_tab = 0;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("概览", self.overview_tab == 0)),
                                    )
                                    .child(
                                        div()
                                            .id("tab-model")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_tab = 1;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("模型", self.overview_tab == 1)),
                                    ),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .h_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .id("range-all")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_range = 0;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("全部", self.overview_range == 0)),
                                    )
                                    .child(
                                        div()
                                            .id("range-30")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_range = 1;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("30 天", self.overview_range == 1)),
                                    )
                                    .child(
                                        div()
                                            .id("range-7")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_range = 2;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("7 天", self.overview_range == 2)),
                                    ),
                            ),
                    )
                    .child(
                        // 统计卡：2 行 × 3 列
                        div()
                            .v_flex()
                            .gap_3()
                            .child(
                                div()
                                    .h_flex()
                                    .gap_3()
                                    .children(stats[..3].iter().map(|c| {
                                        div()
                                            .v_flex()
                                            .gap_1()
                                            .w(px(220.))
                                            .p_3()
                                            .rounded_lg()
                                            .bg(rgb(CARD))
                                            .child(
                                                div()
                                                    .text_size(px(12.5))
                                                    .text_color(rgb(INK_SOFT))
                                                    .child(c.label),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(17.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(INK))
                                                    .child(c.value.clone()),
                                            )
                                    })),
                            )
                            .child(
                                div()
                                    .h_flex()
                                    .gap_3()
                                    .children(stats[3..].iter().map(|c| {
                                        div()
                                            .v_flex()
                                            .gap_1()
                                            .w(px(220.))
                                            .p_3()
                                            .rounded_lg()
                                            .bg(rgb(CARD))
                                            .child(
                                                div()
                                                    .text_size(px(12.5))
                                                    .text_color(rgb(INK_SOFT))
                                                    .child(c.label),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(17.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(INK))
                                                    .child(c.value.clone()),
                                            )
                                    })),
                            ),
                    )
                    .child(grid)
                    .child(
                        div()
                            .pt_1()
                            .text_size(px(13.))
                            .text_color(rgb(INK_SOFT))
                            .child("你的 Token 用量相当于 3,333 本《善恶的彼岸》。"),
                    ),
            )
    }
}
