//! 小组件（chip / 顶部 tab / 分段按钮 / 按钮 / 快捷键提示）。

use super::ChatApp;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 按钮所在的底，决定透明按钮的悬停色：纸底 / 卡片上用 `HOVER`，
/// 沙色底（侧栏、代码块、chip）上用 `SELECTED`。
#[derive(Clone, Copy)]
pub(super) enum Surface {
    Paper,
    Sand,
}

impl ChatApp {
    /// 透明底的按钮（gpui-kit `Button` 的自定义变体）：静止透明、悬停按所在底色出底，前景 `INK_SOFT`。
    /// 只有图标时是方形图标按钮：默认 32 格 16 图标，`.small()` 24 / 14，`.xsmall()` 20 / 12；
    /// 再加 `.label()` 就是图标 + 文字的小按钮（消息下方的复制 / 编辑）。
    pub(super) fn quiet_button(
        id: impl Into<ElementId>,
        icon: IconAll,
        on: Surface,
        cx: &App,
    ) -> Button {
        let hover: Hsla = rgb(match on {
            Surface::Paper => HOVER,
            Surface::Sand => SELECTED,
        })
        .into();
        Button::new(id).icon(icon).cursor_pointer().custom(
            ButtonCustomVariant::new(cx)
                .foreground(rgb(INK_SOFT).into())
                .hover(hover)
                .active(hover),
        )
    }

    /// 文字按钮：`primary` = 赤陶实底白字，否则卡片底 + 细边；配色来自 `theme::apply_colors`。
    /// 放在带边框的卡片里时用 `.with_variant(ButtonVariant::Secondary)`（沙色实底、不描边）。
    pub(super) fn text_button(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        primary: bool,
    ) -> Button {
        Button::new(id).label(label).cursor_pointer().when(primary, |b| b.primary())
    }

    pub(super) fn chip(icon: IconAll, label: impl Into<SharedString>) -> Div {
        div()
            .h_flex()
            .gap_1p5()
            .px_2p5()
            .py_1()
            .rounded_md()
            .bg(rgb(SAND))
            .child(Icon::new(icon).size_3p5().text_color(rgb(INK_SOFT)))
            .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(label.into()))
    }

    /// 消息下方的小操作按钮（复制 / 编辑 / 重跑）：图标 + 元信息字号文字，悬停出底色。
    pub(super) fn message_action(
        id: impl Into<ElementId>,
        icon: IconAll,
        label: impl Into<SharedString>,
        cx: &App,
    ) -> Button {
        Self::quiet_button(id, icon, Surface::Paper, cx).xsmall().label(label).px_2()
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

    /// 快捷键提示（gpui-kit `Kbd`）：按键串传 `keys.rs` 的常量，和绑定同源，改键不用改文案。
    pub(super) fn kbd(key: &str) -> Kbd {
        Kbd::new(Keystroke::parse(key).expect("keys.rs 里的按键串应能解析"))
    }
}
