//! 弹出独立窗（`PopoutWindow`）与跨窗迁移（`PopoutManager` 的那半边：在 `ChatApp` 上）。
//!
//! 一个 Pane 可以活生生地搬到另一个操作系统窗口里，再搬回来：搬运用的是
//! `DockArea::detach_panel`（不触发 `on_removed`，PTY / 虚拟滚动都活着）和
//! `DockArea::add_panel_view_at`（插回原来的位置），见 `vendor/README.md` 的 dock 补丁。
//!
//! 弹出窗是**轻量**的一层：一条 `CHROME_H` 顶栏（标题 + 吸回主窗 + 窗口按钮）加一个
//! `DockArea`。它不跑 `ChatApp`——会话数据从 `WorkspaceHub` 读，所以主窗切会话时
//! 弹出窗里的改动 / 计划 Pane 照样跟着变。

use super::PaneHandle;
use super::skin::ClaudesktopDockSkin;
use crate::app::ChatApp;
use crate::app::chrome::{CHROME_H, drag_area, window_controls};
use crate::theme::rgb;
use crate::theme::*;
use gpui_base::dock::{DockArea, DockAreaRenderer, DockLayout, DockPlacement, PanelId};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;
use std::rc::Rc;

/// 弹出窗默认大小
const POPOUT_SIZE: (Pixels, Pixels) = (px(900.), px(680.));
/// 主窗上记一笔「这个面板现在在哪」，吸回时用
pub(crate) struct PopoutEntry {
    pub(crate) window: AnyWindowHandle,
    /// 弹出窗里那个 dock（吸回时要先把面板从它里面剥出来）
    pub(crate) dock: Entity<DockArea>,
    /// 面板在弹出窗 dock 里的 id（跨窗迁移后 id 不变：还是实体自己的 id）
    pub(crate) panel: PanelId,
    /// 吸回主窗时插回哪个 Tab 组（弹出前它所在的那个），None = 塞进主区第一个组
    pub(crate) home: Option<gpui_base::dock::NodeId>,
}

/// 弹出窗的根视图：轻量顶栏 + 一个 DockArea。
pub(crate) struct PopoutWindow {
    /// 主窗（吸回时回到它那里，也是会话数据的入口）
    main: WeakEntity<ChatApp>,
    dock: Entity<DockArea>,
    title: SharedString,
}

impl PopoutWindow {
    pub(crate) fn new(
        main: WeakEntity<ChatApp>,
        dock: Entity<DockArea>,
        title: SharedString,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self { main, dock, title }
    }
}

impl Render for PopoutWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = self.main.clone();
        let dock = self.dock.clone();
        let reclaim = div()
            .id("popout-reclaim")
            .h_full()
            .flex_shrink_0()
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
            .child(Icon::new(IconAll::PanelRight).small())
            .child("吸回主窗")
            .on_click(cx.listener(move |_, _, window, cx| {
                if let Some(main) = main.upgrade() {
                    let dock = dock.clone();
                    main.update(cx, |app, cx| app.reclaim_popout(dock, window, cx));
                }
            }));

        v_flex()
            .size_full()
            .bg(rgb(PAPER))
            .text_color(rgb(INK))
            .text_size(TEXT_BODY)
            .child(
                div()
                    .h_flex()
                    .h(CHROME_H)
                    .flex_shrink_0()
                    .items_center()
                    .pl_3()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(drag_area().h_flex().items_center().child(self.title.clone()))
                    .child(reclaim)
                    .children(window_controls(window)),
            )
            .child(div().flex_1().min_h_0().w_full().child(self.dock.clone()))
    }
}

impl ChatApp {
    /// 把一个面板搬进独立的操作系统窗口。
    ///
    /// 调用方给的是主窗 dock 里的 `PanelId`；面板本身（`Entity`）跟着 handle 一起过去，
    /// 所以对话草稿、滚动位置、文件树展开状态都不丢。
    pub(crate) fn popout_pane(
        &mut self,
        panel: PanelId,
        at: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 弹出前记一下它原来在哪个 Tab 组：吸回时回到原位，而不是塞进第一个组
        let home = self
            .dock
            .read(cx)
            .layout(DockPlacement::Center)
            .and_then(|tree| tree.find_panel_node(panel));
        let Some(view) = self.dock.update(cx, |dock, cx| dock.detach_panel(panel, window, cx))
        else {
            return;
        };
        let title = match PaneHandle::of(&view) {
            Some(handle) => handle.display_title(cx),
            None => SharedString::from(view.panel_name(cx)),
        };
        let kind = PaneHandle::of(&view).map(|handle| handle.kind(cx));

        let skin = ClaudesktopDockSkin::new();
        let renderer: Rc<dyn DockAreaRenderer> = skin.clone();
        let (bounds_x, bounds_y) = match at {
            Some(point) => (f32::from(point.x), f32::from(point.y)),
            None => (f32::NAN, f32::NAN),
        };
        let mut options = TitleBar::window_options();
        options.focus = true;
        options.window_bounds =
            Some(WindowBounds::Windowed(if bounds_x.is_finite() && bounds_y.is_finite() {
                Bounds {
                    origin: point(px(bounds_x), px(bounds_y)),
                    size: size(POPOUT_SIZE.0, POPOUT_SIZE.1),
                }
            } else {
                Bounds::centered(None, size(POPOUT_SIZE.0, POPOUT_SIZE.1), cx)
            }));
        if let Some(tb) = options.titlebar.as_mut() {
            // 主窗标题保持 `claudesktop`（按标题精准关主窗的逻辑靠它，见 HANDOFF.md）
            tb.title = Some(format!("claudesktop · {title}").into());
        }

        let main = cx.entity().downgrade();
        let slot: Rc<std::cell::RefCell<Option<Entity<DockArea>>>> =
            Rc::new(std::cell::RefCell::new(None));
        let slot_in = slot.clone();
        // 面板句柄进新窗口用这一份，外面那份留着给 `window_changed` 用
        let view_in = view.clone();
        let opened = cx.open_window(options, move |window, cx| {
            let dock = cx.new(|cx| {
                DockArea::new(
                    "popout",
                    Some(super::persist::LAYOUT_SCHEMA_VERSION as usize),
                    window,
                    cx,
                )
                .with_renderer(renderer)
            });
            skin.attach_area(dock.downgrade());
            *slot_in.borrow_mut() = Some(dock.clone());
            let layout = DockLayout::tabs().panel_view(view_in, cx);
            dock.update(cx, |dock, cx| {
                dock.set_center(layout, window, cx);
            });
            let popout_title = title.clone();
            cx.new(|cx| PopoutWindow::new(main, dock, popout_title, cx))
        });

        let Ok(handle) = opened else { return };
        // 换窗口之后要重建绑定旧 `Window` 的输入状态（草稿保留）
        if let Some(handle_ref) = PaneHandle::of(&view) {
            handle_ref.window_changed(window, cx);
        }
        if let Some(dock) = slot.borrow().clone() {
            self.popouts.push(PopoutEntry { window: handle.into(), dock, panel, home });
        }
        // 面板在别处了：主窗这边它的位置由 dock 自己折叠掉
        let _ = kind;
        cx.notify();
    }

    /// 弹出窗里点「吸回主窗」：把面板从那个窗的 dock 里剥出来，插回主窗原来的组。
    pub(crate) fn reclaim_popout(
        &mut self,
        from: Entity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.popouts.iter().position(|entry| entry.dock == from) else {
            return;
        };
        let entry = self.popouts.remove(index);
        let detached = from.update(cx, |dock, cx| dock.detach_panel(entry.panel, window, cx));
        let Some(view) = detached else {
            return;
        };
        // 插回原来的组；那个组已经没了就插进主区的第一个 Tab 组
        let target = self.dock.read(cx).layout(DockPlacement::Center).and_then(|tree| {
            if let Some(node) = entry.home.filter(|node| tree.find_node(*node).is_some()) {
                return Some(gpui_base::dock::InsertTarget::Tabs {
                    node,
                    ix: None,
                    activate: true,
                });
            }
            let mut first = None;
            tree.root().walk(&mut |node| {
                if first.is_none() && matches!(node.kind(), gpui_base::dock::PaneRef::Tabs { .. }) {
                    first = Some(node.id());
                }
            });
            first.map(|node| gpui_base::dock::InsertTarget::Tabs { node, ix: None, activate: true })
        });
        match target {
            Some(target) => {
                self.dock.update(cx, |dock, cx| {
                    dock.add_panel_view_at(view.clone(), target, window, cx)
                });
            }
            None => {
                let layout = DockLayout::tabs().panel_view(view.clone(), cx);
                self.dock.update(cx, |dock, cx| dock.set_center(layout, window, cx));
            }
        }
        if let Some(handle_ref) = PaneHandle::of(&view) {
            handle_ref.window_changed(window, cx);
        }
        // 关掉空出来的弹出窗
        let _ = entry.window.update(cx, |_, window, _| window.remove_window());
        cx.notify();
    }

    /// 主窗关闭前把弹出窗都收掉（否则会留下没有主窗的孤儿窗口）。
    pub(crate) fn close_popouts(&mut self, cx: &mut Context<Self>) {
        for entry in self.popouts.drain(..) {
            let _ = entry.window.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// 用户自己关掉了某个弹出窗：主窗这边的账本跟着划掉（面板随窗口一起没了）。
    pub(crate) fn forget_popout(&mut self, window_id: WindowId, cx: &mut Context<Self>) {
        let before = self.popouts.len();
        self.popouts.retain(|entry| entry.window.window_id() != window_id);
        if self.popouts.len() != before {
            cx.notify();
        }
    }
}
