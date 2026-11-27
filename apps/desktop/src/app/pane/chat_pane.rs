//! 对话 Pane：把原来的主内容区（内容区 + 输入框）装进一个可拖拽的 Panel。
//!
//! 内容区（[`super::super::content::ContentView`]）和循环动画覆盖层是这个 Pane **自己的**
//! 一份，所以同帧渲染时两块内容区互不清空对方的转圈位置（见 `motion::LoopSpots`）。
//! 会话记录本体仍归 `ChatApp`，这里只负责把它画出来。

use super::{ClaudesktopPane, PaneKind};
use crate::app::ChatApp;
use crate::app::content::ContentView;
use crate::app::motion::LoopOverlay;
use gpui_base::dock::{Panel, PanelEvent, PanelState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::*;

pub(crate) struct ChatPane {
    app: WeakEntity<ChatApp>,
    focus_handle: FocusHandle,
    /// 这块对话自己的内容区缓存 view（主窗只有一块时就是 `ChatApp` 的那一份）
    content: Entity<ContentView>,
    /// 叠在内容区上画转圈 / 扫光的覆盖层（和 `content` 共用一份位置池）
    overlay: Entity<LoopOverlay>,
}

impl ChatPane {
    pub(crate) fn new(
        app: WeakEntity<ChatApp>,
        content: Entity<ContentView>,
        overlay: Entity<LoopOverlay>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 跟 hub 事件走：弹出窗里没人替它重画（见 `pane::follow_hub`）
        if let Some(upgraded) = app.upgrade() {
            let hub = upgraded.read(cx).hub.clone();
            super::follow_hub(&hub, cx);
        }
        Self { app, focus_handle: cx.focus_handle(), content, overlay }
    }
}

impl ClaudesktopPane for ChatPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::Chat
    }

    /// Tab 上的标题就是会话标题（切会话时跟着变，见 `ChatApp::sync_hub`）
    fn title(&self, cx: &App) -> SharedString {
        self.app
            .upgrade()
            .map(|app| app.read(cx).session().title.clone())
            .unwrap_or_else(|| PaneKind::Chat.title())
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::MessageSquare
    }

    fn session_token(&self, cx: &App) -> Option<u64> {
        self.app.upgrade().map(|app| app.read(cx).session().token)
    }
}

impl Panel for ChatPane {
    fn panel_name(&self) -> &'static str {
        PaneKind::Chat.panel_name()
    }

    /// 主对话 Pane 不给关：它复用主窗那一份内容区与输入状态实体，
    /// 关掉或弹出都会让主窗失去对话区（要关的是**会话**，走侧栏）。
    fn closable(&self, _cx: &App) -> bool {
        false
    }

    /// 同一个道理：这个 Pane 不能搬窗（见 `popout.rs` 文件头的第 3 条）
    fn zoomable(&self, _cx: &App) -> bool {
        true
    }

    fn dump(&self, _cx: &App) -> PanelState {
        // 一个主对话 Pane：会话由 `ChatApp` 的活跃会话决定，这里不用记
        super::pane_state(PaneKind::Chat, serde_json::json!({ "kind": "chat" }))
    }
}

impl EventEmitter<PanelEvent> for ChatPane {}

impl Focusable for ChatPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().size_full().into_any_element();
        };
        let content = self.content.clone();
        let overlay = self.overlay.clone();
        app.update(cx, |app, cx| app.render_chat_body(content, overlay, window, cx))
    }
}
