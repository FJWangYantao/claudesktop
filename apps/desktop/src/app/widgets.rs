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
use std::cell::Cell;
use std::rc::Rc;

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
    pub(super) fn seg(text: impl Into<SharedString>, active: bool) -> Div {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .text_size(TEXT_SMALL)
            .bg(if active { rgb(SELECTED) } else { rgba(CLEAR) })
            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
            .font_features(tabular_nums())
            .child(text.into())
    }

    /// 快捷键提示（gpui-kit `Kbd`）：按键串传 `keys.rs` 的常量，和绑定同源，改键不用改文案。
    pub(super) fn kbd(key: &str) -> Kbd {
        Kbd::new(Keystroke::parse(key).expect("keys.rs 里的按键串应能解析"))
    }

    /// 按键串的文字写法（如「Ctrl+=」），放进悬停提示这类只收文字的地方。
    pub(super) fn key_text(key: &str) -> String {
        Kbd::format(&Keystroke::parse(key).expect("keys.rs 里的按键串应能解析"))
    }
}

/// 滚动区内容的定宽。gpui 滚一格就重画整个 view，滚动区里的内容宽度不定时，
/// taffy 每帧要按不同约束把整页来回排好几遍（实测设置页每帧 25ms → 定宽后 10ms）。
/// 做法：用 [`probe`](Self::probe) 量出滚动区这一帧的宽度，下一帧拿 [`get`](Self::get) 给内容当定宽；
/// 宽度变了（拉窗口、调字号）就记下并再画一帧。
#[derive(Clone, Default)]
pub(super) struct MeasuredWidth(Rc<Cell<Option<Pixels>>>);

impl MeasuredWidth {
    /// 上一帧量到的宽度（第一帧还没量过是 None，内容照常按不定宽排）。
    pub(super) fn get(&self) -> Option<Pixels> {
        self.0.get()
    }

    /// 量宽的空 canvas：铺满和滚动区同宽的容器（那个容器要 `.relative()`），不挡事件。
    pub(super) fn probe(&self) -> impl IntoElement + use<> {
        let cell = self.0.clone();
        canvas(
            move |bounds, window, _| {
                if cell.get() != Some(bounds.size.width) {
                    cell.set(Some(bounds.size.width));
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
    }
}
