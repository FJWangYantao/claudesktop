//! 弹出独立窗（`PopoutWindow`）与跨窗迁移（在 `ChatApp` 上）。
//!
//! 一个 Pane 可以活生生地搬到另一个操作系统窗口里，再搬回来：搬运用的是
//! `DockArea::detach_panel`（不触发 `on_removed`，虚拟滚动等状态都活着）和
//! `DockArea::add_panel_view_at`（插回原来的位置），见 `vendor/README.md` 的 dock 补丁。
//!
//! 三条不能省的前提：
//!
//! 1. **窗口用 `gpui_kit::open_window` 开**，不要裸调 `cx.open_window`：前者会在根上包
//!    `base::Root`，字号（rem）才跟着设置缩放，`Popover` / `Tooltip` / `Dialog` 才画得出来。
//! 2. **每一步 dock 操作都要在它所属窗口的上下文里做**：把面板往主窗插的时候，手里拿的
//!    `window` 必须换回主窗（`ChatApp::window_handle`），不能拿即将销毁的弹出窗。
//! 3. **主对话 Pane 不参与弹出**：它复用主窗那一份 `ContentView` / 输入状态实体，
//!    同一个实体在两个窗口里画会把滚动、焦点、输入法绑到错窗口上。

use super::skin::ClaudesktopDockSkin;
use super::{PaneHandle, PaneKind};
use crate::app::ChatApp;
use crate::app::chrome::{CHROME_H, window_controls};
use crate::theme::rgb;
use crate::theme::*;
use gpui_base::dock::{DockArea, DockAreaRenderer, DockLayout, DockPlacement, PanelId, PanelView};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

/// 弹出窗默认大小
const POPOUT_SIZE: (Pixels, Pixels) = (px(900.), px(680.));

/// 主窗上记一笔「这个面板现在在哪」，吸回 / 关窗回收时用。
pub(crate) struct PopoutEntry {
    pub(crate) window: AnyWindowHandle,
    /// 弹出窗里那个 dock（吸回时要先把面板从它里面剥出来）
    pub(crate) dock: Entity<DockArea>,
    /// 面板在 dock 里的 id。跨窗迁移后 id 不变：它还是实体自己的 id
    pub(crate) panel: PanelId,
    /// 面板句柄本身。用户直接关掉弹出窗时，靠它把面板捡回来插回主窗
    pub(crate) view: Arc<dyn PanelView>,
    /// 弹出前所在区域（用于原始 Tab 组被清空移除时的回退落点：如底部终端回到 Bottom）
    pub(crate) placement: DockPlacement,
    /// 吸回主窗时插回哪个 Tab 组（弹出前它所在的那个），None = 按 placement 回退
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
        // 吸回：先在本窗里把面板剥出来，之后由主窗接管（见 `ChatApp::reclaim_popout`）
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
                    ChatApp::reclaim_popout(main, dock.clone(), window, cx);
                }
            }));

        v_flex()
            .size_full()
            .bg(rgb(PAPER))
            .text_color(rgb(INK))
            .text_size(TEXT_BODY)
            .font_family(sans())
            .map(|d| {
                // 中文回退字体和主窗一致（根上设一次，子元素继承）
                let mut d = d;
                d.text_style().font_fallbacks = Some(cjk_fallbacks());
                d
            })
            .child(
                div()
                    .h_flex()
                    .h(CHROME_H)
                    .flex_shrink_0()
                    .items_center()
                    .pl_3()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        // 顶栏空白的拖拽区：这里用 `window_control_area`（不是主窗的
                        // `drag_area()`，那条还带 macOS 的窗口移动分支）
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .window_control_area(WindowControlArea::Drag)
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(self.title.clone()),
                            ),
                    )
                    .child(reclaim)
                    .children(window_controls(window)),
            )
            .child(div().flex_1().min_h_0().w_full().child(self.dock.clone()))
    }
}

impl ChatApp {
    /// 把一个面板搬进独立的操作系统窗口。
    ///
    /// 调用方给的是主窗 dock 里的 `PanelId`；面板本身跟着句柄一起过去，所以滚动位置、
    /// 文件树展开状态都不丢。`at` 是**屏幕坐标**（拖出时由主窗把客户区坐标换算过来）。
    ///
    /// 注意：跨窗操作全程不持有 `ChatApp` 的可变借用（`display_title` / `window_changed`
    /// 等回调都会回来读或改 `ChatApp`，持有借用会触发 `double_lease_panic`）。
    pub(crate) fn popout_pane(
        main_entity: Entity<Self>,
        panel: PanelId,
        at: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let dock = main_entity.read(cx).dock.clone();
        // 主对话不弹出：它复用主窗的 `ContentView` / 输入状态实体，两窗共享会互相打架
        let is_chat = dock
            .read(cx)
            .panel(panel)
            .and_then(PaneHandle::of)
            .is_some_and(|handle| handle.kind(cx) == PaneKind::Chat);
        if is_chat {
            return;
        }

        // 弹出前记一下它原来在哪个区域与 Tab 组（四个区域都找）：吸回时回到原位，
        // 而不是塞进主区第一个组
        let mut origin = None;
        for candidate in [
            DockPlacement::Center,
            DockPlacement::Right,
            DockPlacement::Left,
            DockPlacement::Bottom,
        ] {
            let Some(tree) = dock.read(cx).layout(candidate) else { continue };
            if let Some(node) = tree.find_panel_node(panel) {
                origin = Some((candidate, Some(node)));
                break;
            }
        }
        let (placement, home) = origin.unwrap_or((DockPlacement::Center, None));
        let Some(view) = dock.update(cx, |dock, cx| dock.detach_panel(panel, window, cx)) else {
            return;
        };
        // 非中心区的最后一个面板被弹走后，空区域仍占着尺寸且无法再拖出，需要一起移掉
        if placement != DockPlacement::Center && dock.read(cx).is_empty(placement, cx) {
            dock.update(cx, |dock, cx| dock.remove_dock(placement, window, cx));
            if placement == DockPlacement::Right {
                main_entity.update(cx, |app, cx| {
                    app.right_panel.open = false;
                    cx.notify();
                });
            }
        }
        let title = match PaneHandle::of(&view) {
            Some(handle) => handle.display_title(cx),
            None => SharedString::from(view.panel_name(cx)),
        };

        let skin = ClaudesktopDockSkin::new();
        let renderer: Rc<dyn DockAreaRenderer> = skin.clone();
        let mut options = TitleBar::window_options();
        options.focus = true;
        options.window_bounds = Some(WindowBounds::Windowed(match at {
            Some(origin) => Bounds { origin, size: size(POPOUT_SIZE.0, POPOUT_SIZE.1) },
            None => Bounds::centered(None, size(POPOUT_SIZE.0, POPOUT_SIZE.1), cx),
        }));
        if let Some(tb) = options.titlebar.as_mut() {
            // 主窗标题保持 `claudesktop`（按标题精准关主窗的逻辑靠它，见 HANDOFF.md）
            tb.title = Some(format!("claudesktop · {title}").into());
        }

        let main = main_entity.downgrade();
        let slot: Rc<std::cell::RefCell<Option<Entity<DockArea>>>> =
            Rc::new(std::cell::RefCell::new(None));
        let slot_in = slot.clone();
        let view_in = view.clone();
        let opened = gpui_kit::open_window(options, cx, move |window, cx| {
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
            let layout = DockLayout::tabs().panel_view(view_in.clone(), cx);
            dock.update(cx, |dock, cx| dock.set_center(layout, window, cx));
            // 换窗口之后要重建绑定旧 `Window` 的输入状态（草稿保留）：
            // 这里的 `window` 才是新弹出窗
            if let Some(handle) = PaneHandle::of(&view_in) {
                handle.window_changed(window, cx);
            }
            let popout_title = title.clone();
            cx.new(|cx| PopoutWindow::new(main, dock, popout_title, cx))
        });

        let Ok((window_handle, _)) = opened else { return };
        if let Some(popout_dock) = slot.borrow().clone() {
            main_entity.update(cx, |app, cx| {
                app.popouts.push(PopoutEntry {
                    window: window_handle,
                    dock: popout_dock,
                    panel,
                    view,
                    placement,
                    home,
                });
                cx.notify();
            });
        }
    }

    /// 弹出窗里点「吸回主窗」：把面板从那个窗的 dock 里剥出来，插回主窗原来的组。
    ///
    /// `window` 是**弹出窗**——只用来在它的 dock 里做剥离与销毁它自己；往主窗插的时候切回
    /// [`ChatApp::window_handle`]，否则面板会挂给一个马上就销毁的窗口。
    pub(crate) fn reclaim_popout(
        main: Entity<Self>,
        from: Entity<DockArea>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let entry = main.update(cx, |app, _| {
            let index = app.popouts.iter().position(|entry| entry.dock == from)?;
            Some(app.popouts.remove(index))
        });
        let Some(entry) = entry else {
            return;
        };
        let detached = from.update(cx, |dock, cx| dock.detach_panel(entry.panel, window, cx));
        let Some(view) = detached else {
            return;
        };
        Self::adopt_into_main(&main, view, entry.placement, entry.home, cx);
        window.remove_window();
        main.update(cx, |_, cx| cx.notify());
    }

    /// 用户自己关掉了某个弹出窗：主窗这边的账本划掉，**面板捡回主窗**，
    /// 免得最后一个改动 / 计划 Pane 跟着窗口一起消失。
    pub(crate) fn forget_popout(main: Entity<Self>, window_id: WindowId, cx: &mut App) {
        let entry = main.update(cx, |app, _| {
            let index = app.popouts.iter().position(|e| e.window.window_id() == window_id)?;
            Some(app.popouts.remove(index))
        });
        let Some(entry) = entry else {
            return;
        };
        // 窗口已经没了，`detach_panel` 没地方做（也不需要）：句柄还在，直接插回主窗
        Self::adopt_into_main(&main, entry.view, entry.placement, entry.home, cx);
        main.update(cx, |_, cx| cx.notify());
    }

    /// 把面板插回主窗：优先回到它原来那个 Tab 组（四个区域都看），
    /// 那个组没了就按原 `placement` 回建 dock，实在在 Center 且没有组才重建 Center。
    fn adopt_into_main(
        main: &Entity<Self>,
        view: Arc<dyn PanelView>,
        placement: DockPlacement,
        home: Option<gpui_base::dock::NodeId>,
        cx: &mut App,
    ) {
        let (main_window, dock) = {
            let app = main.read(cx);
            (app.window_handle, app.dock.clone())
        };
        let landed_right = std::cell::Cell::new(false);
        let landed_bottom = std::cell::Cell::new(false);
        let _ = main_window.update(cx, |_, window, cx| {
            let panel_id = view.panel_id(cx);
            let target = dock.read(cx).layout(DockPlacement::Center).and_then(|_| {
                let area = dock.read(cx);
                // 原来那个组还在（可能在左 / 右 / 底部）：插回原位
                if let Some(node) = home {
                    let alive = [
                        DockPlacement::Center,
                        DockPlacement::Right,
                        DockPlacement::Left,
                        DockPlacement::Bottom,
                    ]
                    .into_iter()
                    .filter_map(|placement| area.layout(placement))
                    .any(|tree| tree.find_node(node).is_some());
                    if alive {
                        return Some(gpui_base::dock::InsertTarget::Tabs {
                            node,
                            ix: None,
                            activate: true,
                        });
                    }
                }
                None
            });
            if let Some(target) = target {
                dock.update(cx, |dock, cx| {
                    dock.add_panel_view_at(view.clone(), target, window, cx);
                });
            } else if placement != DockPlacement::Center {
                let had_right = dock.read(cx).has_dock(DockPlacement::Right);
                dock.update(cx, |dock, cx| {
                    dock.add_panel_view(view.clone(), placement, None, window, cx);
                    if placement == DockPlacement::Right && !had_right {
                        dock.set_dock_size(
                            DockPlacement::Right,
                            PANEL_DEFAULT_W.to_pixels(window.rem_size()),
                            window,
                            cx,
                        );
                    }
                });
            } else {
                // 否则进中区第一个 Tab 组
                let fallback = dock.read(cx).layout(DockPlacement::Center).and_then(|tree| {
                    let mut first = None;
                    tree.root().walk(&mut |node| {
                        if first.is_none()
                            && matches!(node.kind(), gpui_base::dock::PaneRef::Tabs { .. })
                        {
                            first = Some(node.id());
                        }
                    });
                    first.map(|node| gpui_base::dock::InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: true,
                    })
                });
                match fallback {
                    Some(target) => {
                        dock.update(cx, |dock, cx| {
                            dock.add_panel_view_at(view.clone(), target, window, cx);
                        });
                    }
                    None => {
                        let layout = DockLayout::tabs().panel_view(view.clone(), cx);
                        dock.update(cx, |dock, cx| dock.set_center(layout, window, cx));
                    }
                }
            }
            if dock
                .read(cx)
                .layout(DockPlacement::Right)
                .is_some_and(|tree| tree.find_panel_node(panel_id).is_some())
            {
                dock.update(cx, |dock, cx| {
                    if !dock.is_dock_open(DockPlacement::Right) {
                        dock.toggle_dock(DockPlacement::Right, window, cx);
                    }
                });
                landed_right.set(true);
            }
            if dock
                .read(cx)
                .layout(DockPlacement::Bottom)
                .is_some_and(|tree| tree.find_panel_node(panel_id).is_some())
            {
                dock.update(cx, |dock, cx| {
                    if !dock.is_dock_open(DockPlacement::Bottom) {
                        dock.toggle_dock(DockPlacement::Bottom, window, cx);
                    }
                });
                landed_bottom.set(true);
            }
            // 在**主窗**的上下文里重建绑定窗口的输入状态
            if let Some(handle) = PaneHandle::of(&view) {
                handle.window_changed(window, cx);
            }
        });
        if landed_right.get() {
            main.update(cx, |app, cx| {
                app.right_panel.open = true;
                cx.notify();
            });
        } else if landed_bottom.get() {
            main.update(cx, |_, cx| cx.notify());
        }
    }

    /// 主窗关闭前把弹出窗都收掉（否则会留下没有主窗的孤儿窗口）。
    pub(crate) fn close_popouts(&mut self, cx: &mut Context<Self>) {
        for entry in self.popouts.drain(..) {
            let _ = entry.window.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// 启动时按 `layout.json` 的 `popouts` 把独立窗开回来。
    ///
    /// 面板由注册表按 `panel_name` 重建（不是原来那个实体，但状态在 `dump` 里）；
    /// 在主窗构造完成后调用；仅在登记新窗口时借用主窗。
    pub(crate) fn restore_popouts(
        main_app: &Entity<Self>,
        states: Vec<super::persist::PopoutLayoutState>,
        cx: &mut App,
    ) {
        for state in states {
            let skin = ClaudesktopDockSkin::new();
            let renderer: Rc<dyn DockAreaRenderer> = skin.clone();
            let kind = first_panel_kind(&state.area.center);
            let placement = match kind {
                Some(PaneKind::Terminal) => DockPlacement::Bottom,
                Some(
                    PaneKind::Overview
                    | PaneKind::Files
                    | PaneKind::Diff
                    | PaneKind::Plan
                    | PaneKind::Subagent
                    | PaneKind::Browser,
                ) => DockPlacement::Right,
                _ => DockPlacement::Center,
            };
            // 标题按恢复出来的第一个面板定（和弹出时一样是 `claudesktop · <Pane 标题>`）
            let title: SharedString =
                kind.map(|kind| kind.title()).unwrap_or_else(|| "面板".into());
            let mut options = TitleBar::window_options();
            options.focus = false;
            options.window_bounds = Some(WindowBounds::Windowed(Bounds {
                origin: point(px(state.bounds.x), px(state.bounds.y)),
                size: size(px(state.bounds.width), state_size(state.bounds.height)),
            }));
            if let Some(tb) = options.titlebar.as_mut() {
                tb.title = Some(format!("claudesktop · {title}").into());
            }
            let area_state = state.area.clone();
            let slot: Rc<std::cell::RefCell<Option<Entity<DockArea>>>> =
                Rc::new(std::cell::RefCell::new(None));
            let slot_in = slot.clone();
            let main = main_app.downgrade();
            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
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
                dock.update(cx, |dock, cx| {
                    let _ = dock.load(area_state, window, cx);
                });
                *slot_in.borrow_mut() = Some(dock.clone());
                let popout_title = title.clone();
                cx.new(|cx| PopoutWindow::new(main, dock, popout_title, cx))
            });
            let Ok((window_handle, _)) = opened else { continue };
            let Some(dock) = slot.borrow().clone() else { continue };
            // 忘了记 id / 句柄的话吸回就找不到面板：从刚恢复的 dock 里问回来
            let panel =
                dock.read(cx).layout(DockPlacement::Center).and_then(|tree| tree.panels().next());
            let view = panel.and_then(|id| dock.read(cx).panel(id).cloned());
            if let (Some(panel), Some(view)) = (panel, view) {
                main_app.update(cx, |app, cx| {
                    app.popouts.push(PopoutEntry {
                        window: window_handle,
                        dock,
                        panel,
                        view,
                        placement,
                        home: None,
                    });
                    cx.notify();
                });
            }
        }
    }
}

/// 恢复时的高度做一次下限保护：存进来的如果是 0（窗口被最小化时写的），开出来是个点不动的条
fn state_size(height: f32) -> Pixels {
    px(height.max(320.))
}

/// 存档里第一个面板的种类（弹出窗标题按它写）。
fn first_panel_kind(state: &gpui_base::dock::PanelState) -> Option<PaneKind> {
    if let Some(kind) = PaneKind::from_panel_name(&state.panel_name) {
        return Some(kind);
    }
    state.children.iter().find_map(first_panel_kind)
}
