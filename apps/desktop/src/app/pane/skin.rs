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
        div().id("claudesktop-dock").bg(rgb(PAPER))
    }

    fn split_frame(&self, node: NodeId, _axis: Axis, _: &mut Window, _: &mut App) -> Stateful<Div> {
        div().id(("claudesktop-split", node.as_u64()))
    }

    fn center_frame(&self, _window: &mut Window, _cx: &mut App) -> Stateful<Div> {
        div().id("claudesktop-dock-center")
    }

    /// 分屏之间的那条线：命中区和拖拽都是 base 的，这里只换画法
    /// （1px 细线，拖拽中变成强调色）。
    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<AnyElement> {
        let active = handle.state() == ResizeHandleState::Dragging;
        let color = if active { rgb(CORAL) } else { rgb(LINE) };
        let line = div().bg(color).absolute();
        let line = match handle.axis() {
            Axis::Horizontal => line.top_0().bottom_0().left(px(2.5)).w(px(1.)),
            Axis::Vertical => line.left_0().right_0().top(px(2.5)).h(px(1.)),
        };
        Some(line.into_any_element())
    }

    /// 侧边 dock 顶上一条 1px 分隔线；底部 dock 同理。右侧栏顶部附一张当前会话状态卡。
    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let extent = dock_extent(dock);
        if extent <= px(0.) {
            return content;
        }
        let line = match dock.placement() {
            DockPlacement::Left => div().h_full().w(px(1.)).flex_shrink_0().bg(rgb(LINE)),
            DockPlacement::Right => div().h_full().w(px(1.)).flex_shrink_0().bg(rgb(LINE)),
            DockPlacement::Bottom => div().w_full().h(px(1.)).flex_shrink_0().bg(rgb(LINE)),
            DockPlacement::Center => div(),
        };
        let status_card = (dock.placement() == DockPlacement::Right)
            .then(|| self.main.borrow().as_ref().and_then(|main| main.upgrade()))
            .flatten()
            .and_then(|main| super::workspace::render_right_status_card(main, cx));
        let body = if status_card.is_some() {
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .children(status_card)
                .child(div().flex_1().min_h_0().w_full().child(content))
                .into_any_element()
        } else {
            content
        };
        let frame = match dock.placement() {
            DockPlacement::Bottom => div().v_flex().size_full().overflow_hidden(),
            _ => div().h_flex().size_full().overflow_hidden(),
        };
        frame.child(line).child(body).into_any_element()
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
    fn frame(&self, group: &TabGroupContext, _window: &mut Window, _cx: &mut App) -> Stateful<Div> {
        // 高度由 base 的 `.size_full()` 定，这里只画底：多写一条 `.h()` 会和它打架
        div()
            .id(("claudesktop-tab-group", group.node().as_u64()))
            .v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(PAPER))
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

    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let mut tabs: Vec<AnyElement> = Vec::new();
        for (ix, panel) in group.panels().iter().enumerate() {
            if !panel.visible(cx) {
                continue;
            }
            tabs.push(tab_element(group, panel, ix, &self.area, cx));
        }

        let active_actions = group
            .active_panel()
            .and_then(PaneHandle::of)
            .map(|handle| handle.actions(window, cx))
            .unwrap_or_default();

        let zoomed = group.is_zoomed();
        let node = group.node().as_u64();
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

        // 「弹出独立窗」：把这个 Tab 搬到一个新的操作系统窗口里（Phase 2）。
        // 主对话 Pane 不参与：它复用主窗的输入状态实体（见 popout.rs 文件头）
        let active = group.active_panel();
        let active_id = active.map(|panel| panel.panel_id(cx));
        let active_is_chat =
            active.and_then(PaneHandle::of).is_some_and(|handle| handle.kind(cx) == PaneKind::Chat);
        let main = self.main.borrow().clone();
        let add_button = main
            .as_ref()
            .and_then(|main| main.upgrade())
            .map(|main| super::workspace::render_add_pane_menu(node, main, cx));
        let popout_button = active_id.filter(|_| !active_is_chat).map(move |panel| {
            let main = main.clone();
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
                    if let Some(main) = main.as_ref().and_then(|main| main.upgrade()) {
                        main.update(cx, |app, cx| app.popout_pane(panel, None, window, cx));
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
            .children(tabs)
            .children(add_button)
            // Tab 之后的空白只是空白：**不能**放 `chrome::drag_area()`，
            // 那是给窗口标题栏用的 `WindowControlArea::Drag`，中下部分屏时双击它会最大化窗口
            .child(div().flex_1().min_w_0().h_full())
            .children(active_actions)
            .children(popout_button)
            .child(zoom_button)
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

/// 一个 Tab：图标 + 标题 + badge + 关闭按钮；可拖到别的组或别的窗口。
fn tab_element(
    group: &TabGroupContext,
    panel: &std::sync::Arc<dyn gpui_base::dock::PanelView>,
    ix: usize,
    area: &Rc<RefCell<Option<WeakEntity<DockArea>>>>,
    cx: &mut App,
) -> AnyElement {
    let active = group.active_ix() == ix && !group.is_collapsed();
    let (kind, title, badge, closable) = match PaneHandle::of(panel) {
        Some(handle) => (
            Some(handle.kind(cx)),
            handle.display_title(cx),
            handle.display_badge(cx),
            panel.closable(cx),
        ),
        // base 自己的占位面板（这个版本认不出的 panel_name）：用它的名字兜底
        None => (None, SharedString::from(panel.panel_name(cx)), None, panel.closable(cx)),
    };
    let icon = kind.map_or(IconAll::Square, |kind| kind.icon());
    let panel_id = panel.panel_id(cx);
    let can_close = closable && group.is_panel_closable(panel_id, cx) && !group.is_locked();
    let close_group = group.clone();
    let drag = group.drag_panel(ix, cx);
    // 拖拽开始要告诉 DockArea 一声，它才能在区域外松手时发 DragOutside
    let area = area.clone();
    // 元素 id 要带上组：分屏出两个 Tab 组时，光用下标会撞车
    let node = group.node().as_u64();

    div()
        .id(ElementId::Name(format!("dock-tab-{node}-{ix}").into()))
        .h_full()
        .flex_shrink_0()
        .max_w(rems(13.))
        .h_flex()
        .items_center()
        .gap_1p5()
        .px_2p5()
        .border_r_1()
        .border_color(rgb(LINE))
        .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
        .when(active, |d| d.bg(rgb(SAND_ACTIVE)))
        .when(!active, |d| d.hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK))))
        .child(Icon::new(icon).size_3p5())
        .child(div().min_w_0().overflow_hidden().text_ellipsis().child(title))
        .when_some(badge, |d, badge| {
            d.child(
                div().flex_shrink_0().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child(badge),
            )
        })
        .when(can_close, |d| {
            d.child(
                div()
                    .id(ElementId::Name(format!("dock-tab-close-{node}-{ix}").into()))
                    .flex_shrink_0()
                    .size_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .text_color(rgb(INK_FAINT))
                    .hover(|d| d.bg(rgb(HOVER_STRONG)).text_color(rgb(INK)))
                    .child(Icon::new(IconAll::X).size_3())
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        close_group.close(panel_id, window, cx);
                    }),
            )
        })
        .when(!active, |d| {
            let select = group.clone();
            d.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                select.select_tab(ix, window, cx)
            })
        })
        .when_some(drag, |d, drag| {
            d.on_drag(drag, move |drag, offset, _, cx| {
                drag.set_drag_offset(offset);
                let area = area.borrow().clone();
                if let Some(area) = area {
                    let _ = area.update(cx, |area, _| area.begin_panel_drag(panel_id));
                }
                cx.new(|_| drag.clone())
            })
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    /// 几种 Pane 的 panel_name 能原样读回来，改一个就再也读不回旧布局。
    #[core::prelude::v1::test]
    fn panel_names_round_trip() {
        for kind in [
            super::super::PaneKind::Chat,
            super::super::PaneKind::Diff,
            super::super::PaneKind::Files,
            super::super::PaneKind::Terminal,
        ] {
            assert_eq!(super::super::PaneKind::from_panel_name(kind.panel_name()), Some(kind));
        }
    }
}
