//! 小组件（chip / 顶部 tab / 分段按钮 / 消息操作按钮）。

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

    /// 消息下方的小操作按钮（复制 / 编辑 / 重跑）：图标 + 元信息字号文字，悬停出底色。
    pub(super) fn message_action(
        id: impl Into<ElementId>,
        icon: IconAll,
        label: impl Into<SharedString>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .h_flex()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(rgb(HOVER)))
            .child(Icon::new(icon).size_3().text_color(rgb(INK_SOFT)))
            .child(div().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child(label.into()))
    }

    /// 首页概览的分段切换底（点击逻辑在 overview 的 tab 里）。
    pub(super) fn seg(text: &'static str, active: bool) -> Div {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .text_size(TEXT_SMALL)
            .bg(if active { rgb(SELECTED) } else { rgba(CLEAR) })
            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
            .child(text)
    }
}
