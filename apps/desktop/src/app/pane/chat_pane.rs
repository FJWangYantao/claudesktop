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
    /// 这块对话绑的会话：多块并排时各绑各的（同一 token 最多一块，见 `workspace.rs`）
    token: u64,
    focus_handle: FocusHandle,
    /// 这块对话自己的内容区缓存 view（主窗只有一块时就是 `ChatApp` 的那一份）
    content: Entity<ContentView>,
    /// 叠在内容区上画转圈 / 扫光的覆盖层（和 `content` 共用一份位置池）
    overlay: Entity<LoopOverlay>,
    /// 焦点不在这块时,输入框里的草稿存在这儿（同一时刻只有一块挂着真正的输入框）
    #[allow(dead_code)] // 单活跃 Composer 的草稿热切换（本分支下一笔）
    draft: String,
}

impl ChatPane {
    pub(crate) fn new(
        app: WeakEntity<ChatApp>,
        token: u64,
        content: Entity<ContentView>,
        overlay: Entity<LoopOverlay>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 跟 hub 事件走：弹出窗里没人替它重画（见 `pane::follow_hub`）
        if let Some(upgraded) = app.upgrade() {
            let hub = upgraded.read(cx).hub.clone();
            super::follow_hub(&hub, cx);
        }
        Self { app, token, focus_handle: cx.focus_handle(), content, overlay, draft: String::new() }
    }

    /// 这块对话的内容区（`observe_self` 里要通知它重画，见 `ChatApp::new`）
    pub(crate) fn content(&self) -> Entity<ContentView> {
        self.content.clone()
    }

    /// 这块对话在看哪个会话
    pub(crate) fn token(&self) -> u64 {
        self.token
    }

    /// 换绑会话（侧栏「在右边打开」时用）：内容区跟着换，草稿存回旧会话。
    #[allow(dead_code)]
    pub(crate) fn set_token(&mut self, token: u64, cx: &mut Context<Self>) {
        if self.token == token {
            return;
        }
        self.token = token;
        self.content.update(cx, |view, cx| view.set_token(token, cx));
        cx.notify();
    }
}

impl ClaudesktopPane for ChatPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::Chat
    }

    /// Tab 上的标题就是这块 pane 绑的会话标题（不是全局活跃会话的）
    fn title(&self, cx: &App) -> SharedString {
        self.app
            .upgrade()
            .and_then(|app| {
                let app = app.read(cx);
                app.index_of_token(self.token).map(|idx| app.sessions[idx].title.clone())
            })
            .unwrap_or_else(|| PaneKind::Chat.title())
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::MessageSquare
    }

    fn session_token(&self, _cx: &App) -> Option<u64> {
        Some(self.token)
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
        let token = self.token;
        let content = self.content.clone();
        let overlay = self.overlay.clone();
        app.update(cx, |app, cx| {
            // 焦点在这块才挂真输入框（同一时刻只有一块），其余画占位卡
            let focused = app.focused_chat == Some(token) || app.focused_chat.is_none();
            app.render_chat_pane_view(token, focused, content, overlay, window, cx)
        })
    }
}
