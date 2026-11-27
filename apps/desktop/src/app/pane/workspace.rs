//! 把工作区装进主窗：默认布局、恢复 / 保存、右侧 dock 的开合、以及每个 Pane 的正文。
//!
//! 布局的真相在 `DockArea` 里（`layout/` 里的树）；这里只负责
//! 「建哪些面板」「什么时候读 / 写 `layout.json`」「右侧那一列开着还是收起」。

use super::chat_pane::ChatPane;
use super::diff_pane::DiffPane;
use super::files_pane::FilesPane;
use super::persist;
use super::plan_pane::PlanPane;
use super::subagent_pane::SubagentPane;
use super::{PaneHandle, PaneKind};
use crate::app::ChatApp;
use crate::app::content::ContentView;
use crate::app::motion::LoopOverlay;
use gpui_base::dock::{DockEvent, DockLayout, DockPlacement, PanelView};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;
use std::time::Duration;

/// 拖分割条时 `LayoutChanged` 每帧都来，写盘要防抖（和滚轮落定一个量级）。
const LAYOUT_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);

/// 右侧那一列的默认宽度：沿用原来辅助面板的 460px（`PANEL_DEFAULT_W`），
/// 换算成像素是因为 dock 的尺寸不跟字号缩放走。
fn right_dock_w(window: &Window) -> Pixels {
    crate::theme::PANEL_DEFAULT_W.to_pixels(window.rem_size())
}

impl ChatApp {
    /// 建 dock 的皮肤与面板注册、恢复上次布局、挂写盘订阅。
    /// 由 [`ChatApp::new`] 在会话恢复之后调一次。
    pub(crate) fn init_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 皮肤要反手告诉 dock「哪个面板在拖」（拖出区域松手 → DragOutside），
        // 也要能叫主窗弹窗（Tab 栏上的「弹出独立窗」按钮）
        self.dock_skin.attach_area(self.dock.downgrade());
        self.dock_skin.attach_main(cx.entity().downgrade());

        // 恢复布局时按 panel_name 重建面板：注册表是全局的，主窗每建一次覆盖一次
        for kind in [
            PaneKind::Chat,
            PaneKind::Files,
            PaneKind::Diff,
            PaneKind::SideChat,
            PaneKind::Plan,
            PaneKind::Subagent,
        ] {
            let weak = cx.entity().downgrade();
            gpui_base::dock::register_panel(cx, kind.panel_name(), move |_ctx, window, cx| {
                ChatApp::build_pane(kind, weak.clone(), window, cx)
            });
        }

        // 布局变了就（防抖）写盘；Tab 拖出区域就弹成独立窗
        let dock = self.dock.clone();
        cx.subscribe_in(&dock, window, |this, _, event: &DockEvent, window, cx| match event {
            DockEvent::LayoutChanged => this.schedule_layout_save(cx),
            DockEvent::DragOutside { panel, position } => {
                let (panel, position) = (*panel, *position);
                this.popout_pane(panel, Some(position), window, cx);
            }
            _ => {}
        })
        .detach();

        // 窗口关掉的两种情况：主窗关了 → 把弹出窗一起收掉（别留孤儿窗口）；
        // 某个弹出窗被用户关了 → 从账本里划掉
        let this = cx.entity().downgrade();
        let main_id = window.window_handle().window_id();
        cx.on_window_closed(move |cx, id| {
            let Some(app) = this.upgrade() else { return };
            app.update(cx, |app, cx| {
                if id == main_id {
                    app.close_popouts(cx);
                } else {
                    app.forget_popout(id, cx);
                }
            });
        })
        .detach();

        // 上次的布局优先；读不懂 / 没存过 / 恢复出来一个没有对话 Pane 的布局，都用默认布局。
        // （没有对话 Pane 的窗口等于空壳，宁可丢一次布局）
        let saved = persist::read();
        let restored = saved
            .as_ref()
            .map(|file| {
                let state = file.main.clone();
                self.dock.update(cx, |dock, cx| dock.load(state, window, cx).is_ok())
            })
            .unwrap_or(false);
        if !restored || !self.focus_pane_of_kind(PaneKind::Chat, window, cx) {
            self.install_default_layout(window, cx);
        } else if let Some(file) = saved {
            // 弹出窗等主窗起来之后再开：在构造函数里开新窗口是重入
            let states = file.popouts;
            if !states.is_empty() {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(0)).await;
                    let _ = this.update(cx, |app, cx| app.restore_popouts(states, cx));
                })
                .detach();
            }
        }
        // 右侧那一列跟着 `right_panel.open` 走（会话切到聊天空间时会收起）
        self.sync_right_dock(window, cx);
    }

    /// 内置默认布局：中间一个对话 Pane，右侧文件 / 改动两个 Tab。
    fn install_default_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chat = self.new_chat_pane(cx);
        let files = self.new_files_pane(cx);
        let diff = self.new_diff_pane(cx);
        let dock = self.dock.clone();
        dock.update(cx, |dock, cx| {
            dock.set_center(
                DockLayout::tabs().panel_view(Arc::new(chat) as Arc<dyn PanelView>, cx),
                window,
                cx,
            );
            dock.set_dock(
                DockPlacement::Right,
                DockLayout::tabs()
                    .panel_view(Arc::new(files) as Arc<dyn PanelView>, cx)
                    .panel_view(Arc::new(diff) as Arc<dyn PanelView>, cx),
                window,
                cx,
            );
            dock.set_dock_size(DockPlacement::Right, right_dock_w(window), window, cx);
        });
    }

    /// 按 `panel_name` 重建一个面板（读布局时用）。
    fn build_pane(
        kind: PaneKind,
        app: WeakEntity<ChatApp>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Arc<dyn PanelView> {
        match kind {
            PaneKind::Chat => {
                // 对话 Pane 复用主窗那一份内容区与覆盖层（它们已经挂在 `ChatApp` 上）
                let Some(upgraded) = app.upgrade() else {
                    return Arc::new(PaneHandle::new(cx.new(|cx| FilesPane::new(app, cx))));
                };
                let (content, overlay) = {
                    let app = upgraded.read(cx);
                    (app.content_view.clone(), app.loop_overlay.clone())
                };
                Arc::new(PaneHandle::new(
                    cx.new(|cx| ChatPane::new(app.clone(), content, overlay, cx)),
                ))
            }
            PaneKind::Files => Arc::new(PaneHandle::new(cx.new(|cx| FilesPane::new(app, cx)))),
            PaneKind::Diff => Arc::new(PaneHandle::new(cx.new(|cx| DiffPane::new(app, cx)))),
            PaneKind::Plan => Arc::new(PaneHandle::new(cx.new(|cx| PlanPane::new(app, cx)))),
            PaneKind::Subagent => {
                Arc::new(PaneHandle::new(cx.new(|cx| SubagentPane::new(app, cx))))
            }
            // 其余 Pane 在后续阶段接上；先按文件面板兜底，别让旧布局整份作废
            _ => Arc::new(PaneHandle::new(cx.new(|cx| FilesPane::new(app, cx)))),
        }
    }

    pub(crate) fn new_chat_pane(&mut self, cx: &mut Context<Self>) -> PaneHandle {
        let app = cx.entity().downgrade();
        let content = self.content_view.clone();
        let overlay = self.loop_overlay.clone();
        PaneHandle::new(cx.new(|cx| ChatPane::new(app, content, overlay, cx)))
    }

    pub(crate) fn new_files_pane(&mut self, cx: &mut Context<Self>) -> PaneHandle {
        let app = cx.entity().downgrade();
        PaneHandle::new(cx.new(|cx| FilesPane::new(app, cx)))
    }

    pub(crate) fn new_diff_pane(&mut self, cx: &mut Context<Self>) -> PaneHandle {
        let app = cx.entity().downgrade();
        PaneHandle::new(cx.new(|cx| DiffPane::new(app, cx)))
    }

    pub(crate) fn new_plan_pane(&mut self, cx: &mut Context<Self>) -> PaneHandle {
        let app = cx.entity().downgrade();
        PaneHandle::new(cx.new(|cx| PlanPane::new(app, cx)))
    }

    pub(crate) fn new_subagent_pane(&mut self, cx: &mut Context<Self>) -> PaneHandle {
        let app = cx.entity().downgrade();
        PaneHandle::new(cx.new(|cx| SubagentPane::new(app, cx)))
    }

    /// 按种类新起一个面板（计划 / 子代理这些「按需打开」的 Pane 用）。
    pub(crate) fn new_pane_of_kind(
        &mut self,
        kind: PaneKind,
        cx: &mut Context<Self>,
    ) -> Option<PaneHandle> {
        Some(match kind {
            PaneKind::Chat => self.new_chat_pane(cx),
            PaneKind::Files => self.new_files_pane(cx),
            PaneKind::Diff => self.new_diff_pane(cx),
            PaneKind::Plan => self.new_plan_pane(cx),
            PaneKind::Subagent => self.new_subagent_pane(cx),
            _ => return None,
        })
    }

    /// 打开（或聚焦）某个种类的 Pane：已经在这个工作区里就选中它，
    /// 否则在右侧那一列新起一个 Tab。命令面板 / 快捷键都走这里。
    pub(crate) fn open_pane(
        &mut self,
        kind: PaneKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.focus_pane_of_kind(kind, window, cx) {
            return;
        }
        let Some(handle) = self.new_pane_of_kind(kind, cx) else { return };
        self.right_panel.open = true;
        let dock = self.dock.clone();
        dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                Arc::new(handle) as Arc<dyn PanelView>,
                DockPlacement::Right,
                None,
                window,
                cx,
            );
        });
        self.focus_pane_of_kind(kind, window, cx);
        cx.notify();
    }

    /// 把某个种类的 Pane 显示出来（右侧那一列里可能和别的 Pane 并成 Tab）。
    /// 返回是否找到了它。
    pub(crate) fn focus_pane_of_kind(
        &mut self,
        kind: PaneKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let dock = self.dock.clone();
        let mut found = None;
        // 四个区域都找一遍：Pane 可能被拖到左侧或底部，也可能已经弹到独立窗
        for placement in [
            DockPlacement::Right,
            DockPlacement::Center,
            DockPlacement::Left,
            DockPlacement::Bottom,
        ] {
            let Some(tree) = dock.read(cx).layout(placement) else { continue };
            let ids: Vec<_> = tree.panels().collect();
            for id in ids {
                let handle = dock.read(cx).panel(id).and_then(PaneHandle::of).map(|h| h.kind(cx));
                if handle == Some(kind) {
                    found = Some((dock.clone(), id));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        // 弹出窗里也算「已经在工作区里」：直接在那个窗里把它选中
        if found.is_none() {
            for entry in &self.popouts {
                let ids: Vec<_> = entry
                    .dock
                    .read(cx)
                    .layout(DockPlacement::Center)
                    .map(|tree| tree.panels().collect())
                    .unwrap_or_default();
                for id in ids {
                    let handle =
                        entry.dock.read(cx).panel(id).and_then(PaneHandle::of).map(|h| h.kind(cx));
                    if handle == Some(kind) {
                        found = Some((entry.dock.clone(), id));
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
        }
        if let Some((area, id)) = found {
            area.update(cx, |dock, cx| dock.select_panel(id, window, cx));
            return true;
        }
        false
    }

    /// 右侧那一列：开着就保证 dock 里有这组 Tab 并展开，收起就折起来。
    pub(crate) fn sync_right_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let want = self.right_panel_controls();
        let dock = self.dock.clone();
        let has = dock.read(cx).has_dock(DockPlacement::Right);
        if want && !has {
            let files = self.new_files_pane(cx);
            let diff = self.new_diff_pane(cx);
            dock.update(cx, |dock, cx| {
                dock.set_dock(
                    DockPlacement::Right,
                    DockLayout::tabs()
                        .panel_view(Arc::new(files) as Arc<dyn PanelView>, cx)
                        .panel_view(Arc::new(diff) as Arc<dyn PanelView>, cx),
                    window,
                    cx,
                );
                dock.set_dock_size(DockPlacement::Right, right_dock_w(window), window, cx);
            });
        }
        // 展开 / 收起：只有本区域的 open 状态和想要的不一样才动它（避免每帧写布局）
        let open = dock.read(cx).is_dock_open(DockPlacement::Right);
        if want != open && dock.read(cx).has_dock(DockPlacement::Right) {
            dock.update(cx, |dock, cx| dock.toggle_dock(DockPlacement::Right, window, cx));
        }
    }

    /// 布局写盘：拖分割条时会被每帧叫到，300ms 内的连续变化合成一次写。
    pub(crate) fn schedule_layout_save(&mut self, cx: &mut Context<Self>) {
        if self.layout_save.is_some() {
            return;
        }
        self.layout_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LAYOUT_SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |app, cx| {
                app.layout_save = None;
                let mut file = persist::snapshot(app.dock.read(cx), cx);
                // 弹出窗也记一笔（位置 + 内部布局），下次打开时能照原样摆回去
                for entry in &app.popouts {
                    let area = entry.dock.read(cx).dump(cx);
                    let bounds = entry.window.update(cx, |_, window, _| window.bounds());
                    if let Ok(bounds) = bounds {
                        let bounds = persist::PersistedBounds {
                            x: f32::from(bounds.origin.x),
                            y: f32::from(bounds.origin.y),
                            width: f32::from(bounds.size.width),
                            height: f32::from(bounds.size.height),
                        };
                        file.popouts.push(persist::PopoutLayoutState { bounds, area });
                        // 面板 id 与它原来所在的组另存一份，吸回时用得着
                        let _ = entry.panel;
                    }
                }
                persist::write(&file);
            });
        }));
    }

    /// 命令面板的「重置工作区布局」：丢掉存档，按默认布局重排。
    pub(crate) fn reset_workspace_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dock = self.dock.clone();
        dock.update(cx, |dock, cx| {
            dock.remove_dock(DockPlacement::Right, window, cx);
            dock.set_zoomed_out(window, cx);
        });
        self.install_default_layout(window, cx);
        self.right_panel.open = true;
        self.sync_right_dock(window, cx);
        cx.notify();
    }

    /// 对话 Pane 的正文：首页背景 + 内容区 + 输入框（顶栏在外面，见 chrome.rs）。
    pub(crate) fn render_chat_body(
        &mut self,
        content: Entity<ContentView>,
        overlay: Entity<LoopOverlay>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = v_flex()
            .relative()
            .size_full()
            .min_h_0()
            .w_full()
            .when(self.home_backdrop_active(), |d| d.child(self.home_backdrop_view.clone()))
            .child(self.content_slot_with(content, overlay))
            .when(!self.settings_open, |d| {
                d.child(
                    self.reading_column(div().pb_4().pt_2())
                        .child(self.render_composer(window, cx)),
                )
            });
        body.when(!self.settings_open, |d| self.with_file_drop(d, cx)).into_any_element()
    }
}
