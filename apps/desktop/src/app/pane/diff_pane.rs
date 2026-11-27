//! 改动 Pane：把右侧面板的「改动」工作区（清单 + 逐文件 Diff）装进自由 Pane。
//!
//! 渲染仍走 `right_panel.rs`（Diff 预览、`DiffSelection` 解析都在那边），
//! Phase 3 会把 Scope 分段切换、行内批注、审查页的逐块撤回接到这个 Pane 上。

use super::{ClaudesktopPane, PaneKind};
use crate::app::ChatApp;
use gpui_base::dock::{Panel, PanelEvent, PanelState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::*;

pub(crate) struct DiffPane {
    app: WeakEntity<ChatApp>,
    focus_handle: FocusHandle,
    /// 上一帧量到的面板宽度（双栏 Diff 的阈值按它判）
    width: crate::app::widgets::MeasuredWidth,
}

impl DiffPane {
    pub(crate) fn new(app: WeakEntity<ChatApp>, cx: &mut Context<Self>) -> Self {
        if let Some(upgraded) = app.upgrade() {
            let hub = upgraded.read(cx).hub.clone();
            super::follow_hub(&hub, cx);
        }
        Self { app, focus_handle: cx.focus_handle(), width: Default::default() }
    }
}

impl ClaudesktopPane for DiffPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::Diff
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::FileDiff
    }

    fn session_token(&self, cx: &App) -> Option<u64> {
        self.app.upgrade().map(|app| app.read(cx).session().token)
    }

    /// Tab 上的 badge：这一轮改了几个文件（读缓存，不在这里扫记录）
    fn badge(&self, cx: &App) -> Option<SharedString> {
        let app = self.app.upgrade()?;
        let app = app.read(cx);
        let count = app.right_panel.changed_count;
        (count > 0).then(|| SharedString::from(count.to_string()))
    }
}

impl Panel for DiffPane {
    fn panel_name(&self) -> &'static str {
        PaneKind::Diff.panel_name()
    }

    fn dump(&self, _cx: &App) -> PanelState {
        super::pane_state(PaneKind::Diff, serde_json::json!({ "kind": "diff", "scope": "turn" }))
    }

    /// 切到后台：Diff 预览的虚拟列表缓存放掉（后台 Tab 不该继续占行高缓存）。
    /// 只放自己这一份：文件 Pane 可能正并排开着。
    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if active {
            return;
        }
        if let Some(app) = self.app.upgrade() {
            app.update(cx, |app, _| app.release_diff_list());
        }
    }
}

impl EventEmitter<PanelEvent> for DiffPane {}

impl Focusable for DiffPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DiffPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().size_full().into_any_element();
        };
        let width_rem = self.width.get().map(|w| f32::from(w) / f32::from(window.rem_size()));
        div()
            .relative()
            .size_full()
            .min_h_0()
            .child(self.width.probe())
            .child(app.update(cx, |app, cx| app.render_diff_tab(width_rem, window, cx)))
            .into_any_element()
    }
}
