//! 侧问 Pane（`Ctrl+;`）：围绕主会话提问，**不写入主线、不落盘、关掉即清除**。
//!
//! 实现上是一份「影子会话」+ 一块只画对话流的内容区：
//!
//! - 影子 `Session` 带 `ephemeral`（见 `session.rs`），所有落盘 / 列表 / 空白会话复用入口都绕过它；
//! - 它复用当前会话的 `kernel_key` 与模型，首次发问时才懒连；
//! - 内容区用 [`ContentView::side`]，只画对话流（首页、设置页、审查页、子任务详情都不进来）；
//! - 输入框是这块 Pane **自己的** `InputState`，不抢主窗的 `self.input`；
//! - 关闭时把影子会话从 `sessions` 里删掉（先 `cancel` 掉还在跑的回合）。

use super::{ClaudesktopPane, PaneKind};
use crate::app::ChatApp;
use crate::app::content::ContentView;
use crate::app::motion::{LoopOverlay, LoopSpots};
use crate::theme::rgb;
use crate::theme::*;
use gpui_base::dock::{Panel, PanelEvent, PanelState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub(crate) struct SideChatPane {
    app: WeakEntity<ChatApp>,
    /// 影子会话的令牌（`Session::ephemeral`）
    session: u64,
    focus_handle: FocusHandle,
    content: Entity<ContentView>,
    overlay: Entity<LoopOverlay>,
    /// 这块 Pane 自己的输入框（不抢主窗那份）
    input: Entity<InputState>,
}

/// 侧问输入框的回车发送（两处创建点都要挂：首建和换窗口重建）。
fn subscribe_side_chat_enter(
    input: &Entity<InputState>,
    session: u64,
    window: &mut Window,
    cx: &mut Context<SideChatPane>,
) {
    cx.subscribe_in(
        input,
        window,
        move |this: &mut SideChatPane, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { secondary, .. } if !secondary)
                && let Some(app) = this.app.upgrade()
            {
                app.update(cx, |app, cx| app.send_side_chat(session, window, cx));
            }
        },
    )
    .detach();
}

impl SideChatPane {
    pub(crate) fn new(
        app: WeakEntity<ChatApp>,
        session: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        super::follow_hub(app.clone(), cx);
        let spots = LoopSpots::new();
        let content = cx.new(|_| ContentView::side(app.clone(), session, spots.clone()));
        let overlay = cx.new(|_| LoopOverlay::new(spots));
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("围绕主会话提问…"));
        // 回车即发送（和底栏的箭头按钮等价）
        subscribe_side_chat_enter(&input, session, window, cx);
        Self { app, session, focus_handle: cx.focus_handle(), content, overlay, input }
    }

    /// 这块 Pane 的内容区（`observe_self` 里要通知它重画，见 `ChatApp::new`）
    pub(crate) fn content(&self) -> Entity<ContentView> {
        self.content.clone()
    }

    /// 这块 Pane 的输入框（`ChatApp` 发送时要读它）
    pub(crate) fn input(&self) -> Entity<InputState> {
        self.input.clone()
    }

    /// 换窗口之后重建输入框，保留草稿（绑定旧 `Window` 的 InputState 不能再用）
    fn rebuild_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.input.read(cx).value().to_string();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("围绕主会话提问…"));
        subscribe_side_chat_enter(&input, self.session, window, cx);
        self.input = input;
        if !draft.is_empty() {
            self.input.update(cx, |state, cx| state.set_value(draft, window, cx));
        }
    }
}

impl ClaudesktopPane for SideChatPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::SideChat
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::MessageSquare
    }

    fn session_token(&self, _cx: &App) -> Option<u64> {
        Some(self.session)
    }

    /// 换窗口（弹出 / 吸回）：输入框要跟着新窗口重建
    fn on_window_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rebuild_input(window, cx);
    }
}

impl Panel for SideChatPane {
    fn panel_name(&self) -> &'static str {
        PaneKind::SideChat.panel_name()
    }

    /// 侧问随时可以关：关掉就把影子会话一起丢掉（见 `on_removed`）
    fn closable(&self, _cx: &App) -> bool {
        true
    }

    fn dump(&self, _cx: &App) -> PanelState {
        // 侧问不落盘，布局里也不用记（重启后不会恢复）
        super::pane_state(PaneKind::SideChat, serde_json::json!({ "kind": "side_chat" }))
    }

    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session;
        if let Some(app) = self.app.upgrade() {
            app.update(cx, |app, cx| app.close_side_chat(session, window, cx));
        }
    }
}

impl EventEmitter<PanelEvent> for SideChatPane {}

impl Focusable for SideChatPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SideChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().size_full().into_any_element();
        };
        let session = self.session;
        let content = self.content.clone();
        let overlay = self.overlay.clone();
        let input = self.input.clone();
        app.update(cx, |app, cx| app.render_side_chat(session, content, overlay, input, window, cx))
    }
}

impl ChatApp {
    /// 造一份影子会话（侧问用）：抄当前会话的内核与模型，标 `ephemeral`，不落盘。
    pub(crate) fn spawn_side_chat_session(&mut self, cx: &mut Context<Self>) -> u64 {
        let (kernel, model, cwd, source_token) = {
            let session = self.session();
            (
                session.kernel.clone(),
                session.model_label.clone(),
                session.cwd.clone(),
                session.token,
            )
        };
        let token = self.next_token;
        self.next_token += 1;
        let mut shadow = crate::session::Session::of_kind(
            token,
            kernel,
            cwd,
            claudesktop_core::SessionKind::Chat,
        );
        shadow.ephemeral = true;
        shadow.title = "侧问".into();
        shadow.model_label = model;
        shadow.ready = false;
        self.sessions.push(shadow);
        let shadow_token = self.sessions.last().map(|s| s.token).unwrap_or(token);
        self.side_chat_sources.insert(shadow_token, source_token);
        cx.notify();
        shadow_token
    }

    /// 登记侧问 Pane（`observe_self` 靠它通知内容区重画）
    pub(crate) fn register_side_chat(
        &mut self,
        session: u64,
        pane: WeakEntity<super::side_chat_pane::SideChatPane>,
    ) {
        self.side_chats.insert(session, pane);
    }

    /// 打开侧问（`Ctrl+;`）：给当前会话造一份影子会话，并在中心区右边并排开一块。
    pub(crate) fn open_side_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source_token = self.session().token;
        // 同一主会话已经开着侧问：聚焦那一块，不重复开
        self.side_chats.retain(|_, pane| pane.upgrade().is_some());
        let existing = self
            .side_chat_sources
            .iter()
            .find(|(shadow, src)| **src == source_token && self.side_chats.contains_key(shadow))
            .map(|(shadow, _)| *shadow);
        if let Some(shadow) = existing
            && let Some(pane) = self.side_chats.get(&shadow).and_then(|pane| pane.upgrade())
        {
            let dock = self.dock.clone();
            let id = gpui_base::dock::PanelId::from(pane.entity_id());
            dock.update(cx, |dock, cx| dock.select_panel(id, window, cx));
            return;
        }
        // 主会话的当前配置照抄：同一个内核槽、同一个模型，只是临时
        let shadow_token = self.spawn_side_chat_session(cx);
        let app = cx.entity().downgrade();
        let pane =
            cx.new(|cx| super::side_chat_pane::SideChatPane::new(app, shadow_token, window, cx));
        self.register_side_chat(shadow_token, pane.downgrade());
        let dock = self.dock.clone();
        let root =
            dock.read(cx).layout(gpui_base::dock::DockPlacement::Center).map(|t| t.root().id());
        let handle = std::sync::Arc::new(super::PaneHandle::new(pane))
            as std::sync::Arc<dyn gpui_base::dock::PanelView>;
        dock.update(cx, |dock, cx| match root {
            Some(node) => {
                dock.add_panel_view_at(
                    handle,
                    gpui_base::dock::InsertTarget::Split {
                        node,
                        placement: gpui_base::Placement::Right,
                        size: None,
                    },
                    window,
                    cx,
                );
            }
            None => {
                dock.add_panel_view(
                    handle,
                    gpui_base::dock::DockPlacement::Center,
                    None,
                    window,
                    cx,
                );
            }
        });
        // 侧问不抢 `active` / `focused_chat`：主对话的焦点和草稿一点都不动
        cx.notify();
    }

    /// 侧问 Pane 的正文：对话流 + 自己的输入条。
    pub(crate) fn render_side_chat(
        &mut self,
        session: u64,
        content: Entity<ContentView>,
        overlay: Entity<LoopOverlay>,
        input: Entity<InputState>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = self.with_active(session, |app| app.content_slot_with(content, overlay));
        let busy = self.index_of_token(session).is_some_and(|idx| self.sessions[idx].busy);
        let send =
            div()
                .id(("side-chat-send", session))
                .size_7()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(if busy { HOVER } else { CORAL }))
                .text_color(rgb(if busy { INK_FAINT } else { WHITE }))
                .child(Icon::new(IconAll::ArrowUp).size_4())
                .when(!busy, |d| {
                    d.on_click(cx.listener(move |this, _, window, cx| {
                        this.send_side_chat(session, window, cx)
                    }))
                });
        v_flex()
            .size_full()
            .min_h_0()
            .bg(rgb(PAPER))
            .child(div().relative().flex_1().min_h_0().child(body))
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&input).small().appearance(false)),
                    )
                    .child(send),
            )
            .into_any_element()
    }

    /// 侧问发送：首条消息拼上主会话的精简快照，之后就是普通问答（都发进影子会话）。
    pub(crate) fn send_side_chat(
        &mut self,
        session: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.index_of_token(session) else { return };
        let Some(input) = self
            .side_chats
            .get(&session)
            .and_then(|pane| pane.upgrade())
            .map(|pane| pane.read(cx).input())
        else {
            return;
        };
        let text = input.read(cx).value().trim().to_string();
        if text.is_empty() || self.sessions[idx].busy {
            return;
        }
        // 首条消息带上主会话的上下文。走**文本附件**而不是拼进 `UserPrompt.text`：
        // 附件内容只发给内核，气泡里只显示文件名（拼进正文会把几万字符的
        // `<main-session-context>` 原样画在对话里）
        let attachments = if self.sessions[idx].has_user_message() {
            Vec::new()
        } else {
            let context = self.side_chat_context(session);
            vec![claudesktop_core::attach::TextAttachment {
                name: "主会话上下文.md".into(),
                path: std::path::PathBuf::from("main-session-context.md"),
                text: format!(
                    "<main-session-context>\n{context}\n</main-session-context>\n\n\
                     以上是主会话的上下文。下面只是针对它的旁支提问：只回答问题，\
                     不要修改任何文件或执行改动，用户没有要求就别提议改代码。"
                ),
            }]
        };
        input.update(cx, |state, cx| state.set_value("", window, cx));
        self.submit_to(
            idx,
            claudesktop_core::UserPrompt { text, images: Vec::new(), attachments },
            cx,
        );
    }

    /// 主会话的精简快照：用户消息 + 助手正文 + 工具一行摘要，跳过思考正文与长输出。
    /// 只保留最近 ~24k 字符（超长从头部截断，保住最近几轮）。
    pub(crate) fn side_chat_context(&self, shadow: u64) -> String {
        const LIMIT: usize = 24_000;
        let mut out = String::new();
        let source = self
            .side_chat_sources
            .get(&shadow)
            .and_then(|token| self.index_of_token(*token))
            .map(|idx| &self.sessions[idx]);
        if let Some(source) = source {
            for item in source.transcript.iter() {
                match item {
                    crate::model::ChatItem::User { text, .. } => {
                        out.push_str("\n[用户] ");
                        out.push_str(text.as_ref());
                    }
                    crate::model::ChatItem::Agent(text) => {
                        out.push_str("\n[助手] ");
                        out.push_str(text.as_ref());
                    }
                    crate::model::ChatItem::Tool(card) => {
                        out.push_str("\n[工具 ");
                        out.push_str(card.name.as_ref());
                        out.push_str("] ");
                        out.push_str(card.arg.as_str());
                    }
                    crate::model::ChatItem::Plan(rows) => {
                        out.push_str("\n[计划] ");
                        for row in rows {
                            out.push_str(&format!("{:?} {}；", row.status, row.text));
                        }
                    }
                    _ => {}
                }
            }
        }
        if out.chars().count() > LIMIT {
            let tail: String = out.chars().skip(out.chars().count() - LIMIT).collect();
            return format!("（更早的对话已省略）\n{tail}");
        }
        out
    }

    /// 关掉侧问：取消还在跑的回合，把影子会话从内存里删掉（不落盘的东西一并没了）。
    pub(crate) fn close_side_chat(
        &mut self,
        session: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.side_chats.remove(&session);
        self.side_chat_sources.remove(&session);
        let Some(idx) = self.index_of_token(session) else { return };
        if self.sessions[idx].busy {
            // 先停掉这一轮（和 Esc 停止同一个入口）
            self.with_active(session, |app| app.stop(window, cx));
        }
        if self.sessions[idx].ephemeral {
            self.sessions.remove(idx);
            if idx < self.active {
                self.active = self.active.saturating_sub(1);
            } else if idx == self.active {
                self.active = self.active.min(self.sessions.len().saturating_sub(1));
            }
        }
        cx.notify();
    }
}
