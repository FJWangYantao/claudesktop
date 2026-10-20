//! 弹层菜单公共件：trigger chip 与 Popover 组装。
//!
//! open 状态由 gpui-kit `Popover` 自己持有（点外面 / Esc 关闭），ChatApp 不再
//! 存菜单 bool；条目动作经 `Entity<ChatApp>` 回到根视图，选中后调
//! `PopoverState::dismiss` 收起面板。
//!
//! 条目用 [`MenuList`] 组：前 9 个可选条目右侧标上数字，面板开着时按数字键直接选。
//! 按键要落在焦点链上，所以面板打开时焦点给 `ChatApp::menu_focus`（挂在条目列表外层），
//! 而不是 Popover 自己的外壳（列表是外壳的子元素，不在它的按键分发路径上）。

use super::ChatApp;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::{PopoverState, Selectable};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

/// 下拉 trigger chip：图标 + 文字 + 展开箭头，或只有文字，弹层打开时高亮。
/// 实现 [`Selectable`] 以便作为 `Popover::trigger` 使用。
#[derive(IntoElement)]
pub(super) struct MenuChip {
    icon: Option<IconAll>,
    label: SharedString,
    /// 纯文字（输入框工具栏里的模型）：不带图标和展开箭头，靠悬停底色提示可点
    plain: bool,
    /// 沙色实底（筛选条这类放在纸底上、要一眼看出能点的 chip）
    filled: bool,
    open: bool,
}

impl MenuChip {
    pub(super) fn new(icon: IconAll, label: impl Into<SharedString>) -> Self {
        Self { icon: Some(icon), label: label.into(), plain: false, filled: false, open: false }
    }

    /// 只有文字，不带展开箭头（输入框工具栏里的模型）。
    pub(super) fn plain(label: impl Into<SharedString>) -> Self {
        Self { icon: None, label: label.into(), plain: true, filled: false, open: false }
    }

    /// 沙色实底（筛选条、「添加」这类放在纸底上的 chip）。
    pub(super) fn filled(mut self) -> Self {
        self.filled = true;
        self
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
        let mut chip = div()
            .h_flex()
            .gap_1p5()
            .py_1()
            .rounded_md()
            .map(|d| if self.filled { d.px_2p5().bg(rgb(SAND)) } else { d.px_2() });
        if let Some(icon) = self.icon {
            chip = chip.child(Icon::new(icon).size_3p5().text_color(rgb(INK_SOFT)));
        }
        chip = chip.child(
            // chip 一律次要字号
            div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(self.label),
        );
        if !self.plain {
            chip = chip.child(Icon::new(IconAll::ChevronDown).size_3().text_color(rgb(INK_SOFT)));
        }
        // 沙色底上的悬停 / 打开用 SELECTED，纸底上用 HOVER
        let active = if self.filled { SELECTED } else { HOVER };
        if self.open { chip.bg(rgb(active)) } else { chip.hover(move |d| d.bg(rgb(active))) }
    }
}

/// 增删行数：绿 +N、红 −M，等宽数字。
pub(super) fn diff_stat(added: u32, removed: u32) -> Div {
    div()
        .flex_shrink_0()
        .h_flex()
        .gap_1p5()
        .text_size(TEXT_SMALL)
        .font_features(tabular_nums())
        .child(div().text_color(rgb(DIFF_ADD_FG)).child(format!("+{added}")))
        .child(div().text_color(rgb(DIFF_DEL_FG)).child(format!("−{removed}")))
}

/// 选中一个条目：执行动作后收起面板。点击和数字键共用。
type Pick = Rc<dyn Fn(&mut Window, &mut App)>;

/// 最多几个条目能按数字键选
const MAX_KEYED: usize = 9;

/// 菜单面板里的条目列表。[`pick`](Self::pick) 加可选条目（点了执行动作并收起面板，
/// 前 9 个标数字），[`child`](Self::child) 加别的内容（提示、不收起面板的操作）。
pub(super) struct MenuList {
    handle: Entity<ChatApp>,
    popover: Entity<PopoverState>,
    body: Stateful<Div>,
    picks: Vec<Pick>,
}

impl MenuList {
    pub(super) fn new(
        id: impl Into<ElementId>,
        handle: &Entity<ChatApp>,
        popover: &Entity<PopoverState>,
    ) -> Self {
        Self {
            handle: handle.clone(),
            popover: popover.clone(),
            body: div().id(id).v_flex().gap_0p5(),
            picks: Vec::new(),
        }
    }

    /// 条目多时限高滚动。
    pub(super) fn scroll(mut self, max_h: Rems) -> Self {
        self.body = self.body.max_h(max_h).overflow_y_scroll();
        self
    }

    /// 可选条目：`row` 一般是 [`menu_item`]，`action` 在根视图上执行，之后收起面板。
    pub(super) fn pick(
        self,
        row: Stateful<Div>,
        action: impl Fn(&mut ChatApp, &mut Context<ChatApp>) + 'static,
    ) -> Self {
        self.pick_in(row, move |this, _, cx| action(this, cx))
    }

    /// 同 [`pick`](Self::pick)，动作里要用 `Window`（如给输入框填值）时用它。
    pub(super) fn pick_in(
        mut self,
        row: Stateful<Div>,
        action: impl Fn(&mut ChatApp, &mut Window, &mut Context<ChatApp>) + 'static,
    ) -> Self {
        let (handle, popover) = (self.handle.clone(), self.popover.clone());
        let pick: Pick = Rc::new(move |window, cx| {
            handle.update(cx, |this, cx| action(this, window, cx));
            popover.update(cx, |s, cx| s.dismiss(window, cx));
        });
        let n = self.picks.len() + 1;
        let on_click = pick.clone();
        self.picks.push(pick);
        // 键帽样式，免得和条目里的数字（如增删行数）连成一串
        let row = row
            .on_click(move |_, window, cx| on_click(window, cx))
            .when(n <= MAX_KEYED, |d| d.child(ChatApp::kbd(&n.to_string())));
        self.body = self.body.child(row);
        self
    }

    pub(super) fn child(mut self, child: impl IntoElement) -> Self {
        self.body = self.body.child(child);
        self
    }

    /// 收尾：外层挂上 `focus`（面板打开时焦点在这里），数字键选对应条目。
    pub(super) fn finish(self, focus: &FocusHandle) -> Div {
        let picks = self.picks;
        div().track_focus(focus).child(self.body).on_key_down(move |event, window, cx| {
            let keys = &event.keystroke;
            // 只放行 Shift：AZERTY 等布局上打数字要按住 Shift，`key` 仍是 "1"。
            // Ctrl / Alt 要留给 Ctrl+K、Alt+1..4 这些快捷键
            if keys.modifiers.control || keys.modifiers.alt || keys.modifiers.platform {
                return;
            }
            let n = keys.key.parse::<usize>().ok().filter(|n| (1..=MAX_KEYED).contains(n));
            if let Some(pick) = n.and_then(|n| picks.get(n - 1)) {
                pick(window, cx);
                cx.stop_propagation();
            }
        })
    }
}

/// 两行式菜单条目：图标 + 名称（+ 灰色说明，空串不显示），当前项右侧打赤陶色对勾。
/// 交给 [`MenuList::pick`]，点击动作由它挂。
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
            // min_w_0：长说明（如路径）在面板宽度内换行，不撑出去
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_size(TEXT_BODY).text_color(rgb(INK)).child(name.into()))
                .when(!desc.is_empty(), |d| {
                    d.child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(desc))
                }),
        )
        .when(active, |d| d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL))))
}

/// 字体预览行用的样例字：拉丁字母（字宽 / 字形差异最明显）、数字、中文
/// （缺字形时走回退，和真正用起来的观感一致）。
pub(super) const FONT_PREVIEW: &str = "永远保持好奇 Aa 123";

/// 字体菜单条目：上面一行灰字写名称（界面字体排，始终读得清），下面用**这个字体本身**
/// 排一行预览——挑字体看的是字形，不是一个名字。行尾当前项打勾。
///
/// `family` 传字体族原名（内嵌的 Inter 也传它的族名，别用 `sans()`：选着别的字体时
/// 那一项的预览会跟着变，就不成参照了）。
pub(super) fn font_menu_item(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    family: impl Into<SharedString>,
    sample: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    let family: SharedString = family.into();
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
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .child(name.into()),
                )
                .child(
                    // 预览行：中文缺字形时按根视图的中文回退字体排，和真正用起来的观感一致
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(family)
                        .text_size(TEXT_LEAD)
                        .text_color(rgb(INK))
                        .child(sample.into()),
                ),
        )
        .when(active, |d| {
            d.child(Icon::new(IconAll::Check).size_4().flex_shrink_0().text_color(rgb(CORAL)))
        })
}

/// 配色菜单条目：上面一行名称 + 灰色说明，下面一行色块预览
/// （纸底 / 侧栏 / 卡片 / 强调 / 正文），行尾当前项打勾。
/// `palette` 是这套色板在 `theme::PALETTES` 里的下标，色块经 `theme::swatch` 取色。
pub(super) fn theme_menu_item(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    desc: impl Into<SharedString>,
    palette: usize,
    active: bool,
) -> Stateful<Div> {
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
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().text_size(TEXT_BODY).text_color(rgb(INK)).child(name.into()))
                .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(desc.into()))
                .child(div().h_flex().gap_1().children([PAPER, SAND, CARD, CORAL, INK].map(|c| {
                    div()
                        .size_3p5()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(LINE))
                        .bg(crate::theme::swatch(palette, c))
                }))),
        )
        .when(active, |d| {
            d.child(Icon::new(IconAll::Check).size_4().flex_shrink_0().text_color(rgb(CORAL)))
        })
}

/// 组一个下拉菜单：`chip` 是 trigger，面板锚在 chip 上按 `anchor` 方向展开，
/// 宽 `width`（rem，跟着界面字号缩放，否则放大字号后说明文字挤得折行）。`items` 每次渲染面板时调用——`app` 是当前状态快照（读数据），
/// `handle` / `popover` 交给 [`MenuList::new`]。`focus` 传 `ChatApp::menu_focus`。
/// 返回的 `Popover` 还可以继续链 `.on_open_change` 等配置。
pub(super) fn menu_popover(
    id: impl Into<ElementId>,
    anchor: Anchor,
    width: Rems,
    handle: &Entity<ChatApp>,
    focus: &FocusHandle,
    chip: MenuChip,
    items: impl Fn(&ChatApp, &Entity<ChatApp>, &Entity<PopoverState>) -> MenuList + 'static,
) -> Popover {
    let handle = handle.clone();
    let list_focus = focus.clone();
    menu_panel(Popover::new(id), focus).anchor(anchor).w(width).trigger(chip).content(
        move |_, _, cx| {
            let popover = cx.entity();
            items(handle.read(cx), &handle, &popover).finish(&list_focus)
        },
    )
}

/// 菜单面板的外观和焦点（trigger 不是 [`MenuChip`] 的菜单自己组 Popover 时也用它）。
pub(super) fn menu_panel(popover: Popover, focus: &FocusHandle) -> Popover {
    popover.track_focus(focus).bg(rgb(CARD)).border_1().border_color(rgb(LINE)).rounded_lg().p_1()
}
