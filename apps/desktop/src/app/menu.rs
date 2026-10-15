//! 弹层菜单公共件：trigger chip 与 Popover 组装。
//!
//! open 状态由 gpui-kit `Popover` 自己持有（点外面 / Esc 关闭），ChatApp 不再
//! 存菜单 bool；条目动作经 `Entity<ChatApp>` 回到根视图，选中后调
//! `PopoverState::dismiss` 收起面板。

use super::ChatApp;
use crate::theme::*;
use crate::theme::rgb;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::{PopoverState, Selectable};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 下拉 trigger chip：图标（可省）+ 文字 + 展开箭头，弹层打开时高亮。
/// 实现 [`Selectable`] 以便作为 `Popover::trigger` 使用。
#[derive(IntoElement)]
pub(super) struct MenuChip {
    icon: Option<IconAll>,
    label: SharedString,
    open: bool,
}

impl MenuChip {
    pub(super) fn new(icon: IconAll, label: impl Into<SharedString>) -> Self {
        Self { icon: Some(icon), label: label.into(), open: false }
    }

    /// 只有文字（模型 chip）。
    pub(super) fn text(label: impl Into<SharedString>) -> Self {
        Self { icon: None, label: label.into(), open: false }
    }
}

impl Selectable for MenuChip {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for MenuChip {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut chip = div().h_flex().gap_1p5().px_2().py_1().rounded_md();
        if let Some(icon) = self.icon {
            chip = chip.child(Icon::new(icon).size_3p5().text_color(rgb(INK_SOFT)));
        }
        chip = chip
            .child(
                // chip 一律次要字号，与输入框上方的环境 / 项目 chip 同档
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(self.label),
            )
            .child(Icon::new(IconAll::ChevronDown).size_3().text_color(rgb(INK_SOFT)));
        if self.open {
            chip.bg(rgb(HOVER))
        } else {
            chip.hover(|d| d.bg(rgb(HOVER)))
        }
    }
}

/// 条目点击的公共收尾：执行动作后收起面板。
macro_rules! menu_action {
    ($handle:expr, $popover:expr, $body:expr) => {
        move |_, window, cx: &mut App| {
            $handle.update(cx, $body);
            $popover.update(cx, |s, cx| s.dismiss(window, cx));
        }
    };
}
pub(super) use menu_action;

/// 两行式菜单条目：图标 + 名称（+ 灰色说明，空串不显示），当前项右侧打赤陶色对勾。
/// 点击动作由调用方链 `.on_click(menu_action!(..))`。
pub(super) fn menu_item(
    id: impl Into<ElementId>,
    icon: IconAll,
    name: impl Into<SharedString>,
    desc: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    let desc: SharedString = desc.into();
    div()
        .id(id)
        .h_flex()
        .items_center()
        .gap_2p5()
        .px_2p5()
        .py_2()
        .rounded_md()
        .cursor_pointer()
        .hover(|d| d.bg(rgb(HOVER)))
        .child(Icon::new(icon).size_4().text_color(rgb(INK_SOFT)))
        .child(
            div()
                .v_flex()
                .flex_1()
                .child(div().text_size(TEXT_BODY).text_color(rgb(INK)).child(name.into()))
                .when(!desc.is_empty(), |d| {
                    d.child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(desc))
                }),
        )
        .when(active, |d| d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL))))
}

/// 组一个下拉菜单：`chip` 是 trigger，面板锚在 chip 上按 `anchor` 方向展开，
/// 宽 `width`。`items` 每次渲染面板时调用——`app` 是当前状态快照（读数据），
/// `handle` 用来在条目点击里更新根视图，`popover` 用来在选中后收起面板。
/// 返回的 `Popover` 还可以继续链 `.on_open_change` 等配置。
pub(super) fn menu_popover<E: IntoElement + 'static>(
    id: &'static str,
    anchor: Anchor,
    width: Pixels,
    handle: &Entity<ChatApp>,
    chip: MenuChip,
    items: impl Fn(&ChatApp, &Entity<ChatApp>, &Entity<PopoverState>) -> E + 'static,
) -> Popover {
    let handle = handle.clone();
    Popover::new(id)
        .anchor(anchor)
        .w(width)
        .trigger(chip)
        .bg(rgb(CARD))
        .border_1()
        .border_color(rgb(LINE))
        .rounded_lg()
        .p_1()
        .content(move |_, _, cx| {
            let popover = cx.entity();
            items(&handle.read(cx), &handle, &popover)
        })
}
