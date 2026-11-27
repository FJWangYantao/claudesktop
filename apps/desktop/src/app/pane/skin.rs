//! `ClaudesktopDockSkin`：`gpui_base::dock` 的外观层（底座一点颜色都不画）。
//!
//! 配色 / 尺寸全部走 `theme.rs`（`PAPER` / `SAND_ACTIVE` / `HOVER` / `LINE` / `INK*`）与 `Rems`，
//! 和右侧面板、侧栏的观感对齐。这里只做外观与命中区：
//! 选中 / 关闭 / zoom / 拖拽的行为都是 `TabGroupContext` 的回调，分屏拖动是 base 自己的。

use super::{PaneHandle, PaneKind};
use crate::theme::rgb;
use crate::theme::*;
use gpui_base::dock::{
    DockArea, DockAreaRenderer, DockContext, DockPlacement, DropIndicator, NodeId, TabGroupContext,
    TabGroupRenderer, dock_extent,
};
use gpui_base::{ResizeHandleContext, ResizeHandleState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Tab 栏高度（和右侧面板、侧栏头部一致的一档）
const TAB_STRIP_H: Rems = rems(2.25);

pub(crate) struct ClaudesktopDockSkin {
    /// 反手拿 `DockArea` 的弱引用：Tab 开始拖时告诉它「哪个面板在拖」，
    /// 拖出区域松手才能变成 `DockEvent::DragOutside`（见 vendor 的补丁说明）。
    /// 用 `Rc<RefCell<_>>` 而不是普通字段：Tab 组渲染器要拿同一份。
    area: Rc<RefCell<Option<WeakEntity<DockArea>>>>,
    /// 主窗：Tab 栏上的「弹出独立窗」按钮从这里叫（皮肤只拿得到 `&mut App`）。
    main: Rc<RefCell<Option<WeakEntity<crate::app::ChatApp>>>>,
}

impl ClaudesktopDockSkin {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self { area: Rc::new(RefCell::new(None)), main: Rc::new(RefCell::new(None)) })
    }

    /// 区域建好之后回填（`with_renderer` 要拿走 Rc，只能建完再告诉它）。
    pub(crate) fn attach_area(&self, area: WeakEntity<DockArea>) {
        *self.area.borrow_mut() = Some(area);
    }

    /// 主窗建好之后回填（弹出按钮要用）。
    pub(crate) fn attach_main(&self, main: WeakEntity<crate::app::ChatApp>) {
        *self.main.borrow_mut() = Some(main);
    }
}

impl DockAreaRenderer for ClaudesktopDockSkin {
    fn frame(&self, _window: &mut Window, _cx: &mut App) -> Stateful<Div> {
        div().id("claudesktop-dock")
    }

    fn split_frame(&self, node: NodeId, _axis: Axis, _: &mut Window, _: &mut App) -> Stateful<Div> {
        div().id(("claudesktop-split", node.as_u64()))
    }

    fn center_frame(&self, _window: &mut Window, _cx: &mut App) -> Stateful<Div> {
        div().id("claudesktop-dock-center")
    }

    /// 分屏之间的那条线：静止时完全隐形，让对话区和右侧卡片融在同一块画布里；
    /// 悬停时显出 1px 细线提示可拖宽度，拖拽中变成强调色。
    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<AnyElement> {
        let color = match handle.state() {
            ResizeHandleState::Idle => return None,
            ResizeHandleState::Hovered => rgb(LINE),
            _ => rgb(CORAL),
        };
        let line = div().bg(color).absolute();
        let line = match handle.axis() {
            Axis::Horizontal => line.top_0().bottom_0().left(px(2.5)).w(px(1.)),
            Axis::Vertical => line.left_0().right_0().top(px(2.5)).h(px(1.)),
        };
        Some(line.into_any_element())
    }

    /// 只有底部终端保留顶上 1px 分隔线；左右面板直接与中间画布融在一起，不再竖线割裂。
    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        _window: &mut Window,
        _cx: &mut App,
    ) -> AnyElement {
        let extent = dock_extent(dock);
        if extent <= px(0.) || dock.placement() != DockPlacement::Bottom {
            return content;
        }
        div()
            .v_flex()
            .size_full()
            .overflow_hidden()
            .child(div().w_full().h(px(1.)).flex_shrink_0().bg(rgb(LINE)))
            .child(content)
            .into_any_element()
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(ClaudesktopTabGroup { area: self.area.clone(), main: self.main.clone() })
    }
}

/// Tab 组的外观：Tab 栏 + 面板区域的排布。
struct ClaudesktopTabGroup {
    area: Rc<RefCell<Option<WeakEntity<DockArea>>>>,
    main: Rc<RefCell<Option<WeakEntity<crate::app::ChatApp>>>>,
}

impl TabGroupRenderer for ClaudesktopTabGroup {
    fn frame(&self, group: &TabGroupContext, _window: &mut Window, cx: &mut App) -> Stateful<Div> {
        let kind = group.active_panel().and_then(PaneHandle::of).map(|pane| pane.kind(cx));
        // 高度由 base 的 `.size_full()` 定，这里只画底：多写一条 `.h()` 会和它打架
        div()
            .id(("claudesktop-tab-group", group.node().as_u64()))
            .v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            // 对话和概览自己按会话状态决定是否露出工作区底层。
            .when(!matches!(kind, Some(PaneKind::Chat | PaneKind::Overview)), |d| d.bg(rgb(PAPER)))
            .text_size(TEXT_SMALL)
    }

    fn content_frame(
        &self,
        group: &TabGroupContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Stateful<Div> {
        div()
            .id(("claudesktop-tab-content", group.node().as_u64()))
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
    }

    /// 去掉传统多 Tab 栏：
    /// - 右侧「概览」卡片栈与唯一主对话完全无栏（0px）；
    /// - 点进详情页（改动 / 文件 / 计划 / 子代理 / 浏览器）或独立分屏（终端 / 侧问 / 第 2 块对话）时，
    ///   只画一条同画布底色的轻页头：左侧 `‹ 概览` 返回 + 当前视图标题（可拖），右侧专属控件 / 弹出 / 放大 / 关闭。
    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let Some(active) = group.active_panel() else {
            return Empty.into_any_element();
        };
        let handle = PaneHandle::of(active);
        let kind = handle.map(|h| h.kind(cx));
        let panel_id = active.panel_id(cx);
        let can_close =
            active.closable(cx) && group.is_panel_closable(panel_id, cx) && !group.is_locked();
        if matches!(kind, Some(PaneKind::Overview))
            || (matches!(kind, Some(PaneKind::Chat)) && !can_close)
        {
            return Empty.into_any_element();
        }

        let node = group.node().as_u64();
        // 同组里有「概览」说明这是右侧检视区钻进来的详情页：给一个一键退回按钮
        let overview_ix = group.panels().iter().position(|panel| {
            PaneHandle::of(panel).is_some_and(|h| h.kind(cx) == PaneKind::Overview)
        });
        let back_button = overview_ix.map(|ix| {
            let select = group.clone();
            div()
                .id(("claudesktop-pane-back", node))
                .h_full()
                .flex_shrink_0()
                .px_2()
                .h_flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                .child(Icon::new(IconAll::ChevronLeft).small())
                .child("概览")
                .on_click(move |_, window, cx| select.select_tab(ix, window, cx))
        });

        let title = handle
            .map(|h| h.display_title(cx))
            .unwrap_or_else(|| SharedString::from(active.panel_name(cx)));
        let badge = handle.and_then(|h| h.display_badge(cx));
        let icon = kind.map_or(IconAll::Square, PaneKind::icon);
        let active_ix = group.active_ix();
        let drag = group.drag_panel(active_ix, cx);
        let area = self.area.clone();
        let title_pill = div()
            .id(("claudesktop-pane-title", node))
            .h_full()
            .min_w_0()
            .px_2()
            .h_flex()
            .items_center()
            .gap_1p5()
            .text_size(TEXT_SMALL)
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(INK))
            .child(Icon::new(icon).size_3p5().text_color(rgb(INK_SOFT)))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().child(title))
            .when_some(badge, |d, badge| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .font_features(tabular_nums())
                        .child(badge),
                )
            })
            .when_some(drag, |d, drag| {
                d.on_drag(drag, move |drag, offset, _, cx| {
                    drag.set_drag_offset(offset);
                    if let Some(area) = area.borrow().clone() {
                        let _ = area.update(cx, |area, _| area.begin_panel_drag(panel_id));
                    }
                    cx.new(|_| drag.clone())
                })
            });

        let active_actions = handle.map(|h| h.actions(window, cx)).unwrap_or_default();
        let zoomed = group.is_zoomed();
        let zoom_handle = group.clone();
        let zoom_button = div()
            .id(("claudesktop-tab-zoom", node))
            .h_full()
            .flex_shrink_0()
            .px_1p5()
            .flex()
            .items_center()
            .text_color(rgb(INK_SOFT))
            .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
            .child(Icon::new(if zoomed { IconAll::Minimize } else { IconAll::Maximize }).small())
            .on_click(move |_, window, cx| {
                zoom_handle.toggle_zoom(window, cx);
            });

        // 主对话 Pane 不参与弹出独立窗：它复用主窗的输入状态实体（见 popout.rs）；
        // 已经在独立窗里的面板也不再套娃显示「弹出独立窗」与「关闭」按钮（顶部已有「吸回主窗」）
        let active_is_chat = kind == Some(PaneKind::Chat);
        let main_weak = self.main.borrow().clone();
        let is_main = main_weak.is_some();
        let popout_button = (!active_is_chat && is_main).then(|| {
            let main_weak = main_weak.clone();
            div()
                .id(("claudesktop-tab-popout", node))
                .h_full()
                .flex_shrink_0()
                .px_1p5()
                .flex()
                .items_center()
                .text_color(rgb(INK_SOFT))
                .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                .child(Icon::new(IconAll::ArrowUpRight).small())
                .on_click(move |_, window, cx| {
                    if let Some(main) = main_weak.as_ref().and_then(|main| main.upgrade()) {
                        crate::app::ChatApp::popout_pane(main, panel_id, None, window, cx);
                    }
                })
        });

        // 非中心区最后一个面板（如底部终端）的 `is_panel_closable` 会被 `TabGroup::is_last_panel`
        // 判成 false，导致关闭按钮不画、且 `group.close` 为空操作；关掉这类面板需要连 dock 一并移除
        let sole_dock_placement =
            (!can_close && active.closable(cx) && !group.is_locked() && is_main)
                .then(|| {
                    let area = self.area.borrow().as_ref()?.upgrade()?;
                    let area = area.read(cx);
                    [DockPlacement::Bottom, DockPlacement::Left, DockPlacement::Right]
                        .into_iter()
                        .find(|&placement| {
                            area.layout(placement)
                                .is_some_and(|tree| tree.find_panel_node(panel_id).is_some())
                        })
                })
                .flatten();

        let close_group = group.clone();
        let close_area = self.area.borrow().clone();
        let close_main = main_weak;
        // 概览组里的固定视图（文件 / 改动 / 计划 / 子代理）只靠「‹ 概览」返回，不直接从组里删掉；
        // 临时 / 独立面板（浏览器 / 终端 / 侧问 / 多开对话）给一个关闭按钮
        let show_close = (can_close || sole_dock_placement.is_some())
            && !matches!(
                kind,
                Some(
                    PaneKind::Overview
                        | PaneKind::Files
                        | PaneKind::Diff
                        | PaneKind::Plan
                        | PaneKind::Subagent
                )
            );
        let close_button = show_close.then(|| {
            div()
                .id(("claudesktop-pane-close", node))
                .h_full()
                .flex_shrink_0()
                .px_1p5()
                .flex()
                .items_center()
                .text_color(rgb(INK_SOFT))
                .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                .child(Icon::new(IconAll::X).small())
                .on_click(move |_, window, cx| {
                    if let Some(placement) = sole_dock_placement {
                        if let Some(area) = close_area.as_ref().and_then(|area| area.upgrade()) {
                            area.update(cx, |dock, cx| dock.remove_dock(placement, window, cx));
                        }
                        if placement == DockPlacement::Right
                            && let Some(main) = close_main.as_ref().and_then(|main| main.upgrade())
                        {
                            main.update(cx, |app, cx| {
                                app.right_panel.open = false;
                                cx.notify();
                            });
                        }
                    } else {
                        close_group.close(panel_id, window, cx);
                    }
                })
        });

        div()
            .h_flex()
            .h(TAB_STRIP_H.to_pixels(window.rem_size()))
            .w_full()
            .flex_shrink_0()
            .items_center()
            .border_b_1()
            .border_color(rgb(LINE))
            .bg(rgb(PAPER))
            .overflow_hidden()
            .children(back_button)
            .child(title_pill)
            .child(div().flex_1().min_w_0().h_full())
            .children(active_actions)
            .children(popout_button)
            .child(zoom_button)
            .children(close_button)
            .into_any_element()
    }

    fn render_active_panel(
        &self,
        panel: AnyView,
        _group: &TabGroupContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> AnyElement {
        div().size_full().min_h_0().child(panel).into_any_element()
    }

    /// 拖到某个 Tab 组上：落点那半区用强调色的低透明度填充、边框标出。
    ///
    /// 坐标必须是**相对本组内容区**的：这个元素被塞进 `content_frame` 里当子元素，
    /// 而 `indicator.bounds()` 是窗口绝对坐标（右侧 / 下方分组会整个偏出面板）。
    /// 所以只取 `placement`（落点是左/右/上/下/合并），自己算相对矩形。
    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<AnyElement> {
        let overlay =
            div().absolute().bg(with_alpha(rgb(CORAL), 0.10)).border_1().border_color(rgb(CORAL));
        let overlay = match indicator.placement() {
            Some(Placement::Left) => overlay.left_0().top_0().w(relative(0.5)).h_full(),
            Some(Placement::Right) => overlay.right_0().top_0().w(relative(0.5)).h_full(),
            Some(Placement::Top) => overlay.left_0().top_0().w_full().h(relative(0.5)),
            Some(Placement::Bottom) => overlay.left_0().bottom_0().w_full().h(relative(0.5)),
            // 没有方位 = 并进这个 Tab 组：整块高亮
            None => overlay.left_0().top_0().size_full(),
        };
        Some(overlay.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    /// 几种 Pane 的 panel_name 能原样读回来，改一个就再也读不回旧布局。
    #[core::prelude::v1::test]
    fn panel_names_round_trip() {
        for kind in [
            super::super::PaneKind::Chat,
            super::super::PaneKind::SideChat,
            super::super::PaneKind::Overview,
            super::super::PaneKind::Diff,
            super::super::PaneKind::Files,
            super::super::PaneKind::Plan,
            super::super::PaneKind::Subagent,
            super::super::PaneKind::Terminal,
            super::super::PaneKind::Browser,
        ] {
            assert_eq!(super::super::PaneKind::from_panel_name(kind.panel_name()), Some(kind));
        }
    }
}
