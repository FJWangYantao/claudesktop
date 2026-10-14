//! 小组件（chip / 顶部 tab / 分段按钮）。

use super::ChatApp;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;

impl ChatApp {
    pub(super) fn chip(icon: IconAll, label: impl Into<SharedString>) -> Div {
        div()
            .h_flex()
            .gap_1p5()
            .px_2p5()
            .py_1()
            .rounded_md()
            .bg(rgb(SAND))
            .child(
                Icon::new(icon)
                    .size_3p5()
                    .text_color(rgb(INK_SOFT)),
            )
            .child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(label.into()),
            )
    }

    /// 首页概览的分段切换底（点击逻辑在 overview 的 tab 里）。
    pub(super) fn seg(text: &'static str, active: bool) -> Div {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .text_size(TEXT_SMALL)
            .bg(if active { rgb(SAND_ACTIVE) } else { rgba(CLEAR) })
            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
            .child(text)
    }
}
