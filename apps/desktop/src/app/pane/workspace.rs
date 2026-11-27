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

/// 一块对话 Pane 不聚焦时的输入草稿：文本 + 待发附件。
///
/// 同一时刻只有一块挂真输入框（`ChatApp::input`），其余存这里。用户切过来时热换进去，
/// 这样两个并排的 Pane 不会在同一帧里各自挂一份 `TextareaState`（光标 / 选区 / IME 会打架）。
pub(crate) struct ChatDraft {
    text: String,
    pending: Vec<crate::model::PendingImage>,
    pending_files: Vec<claudesktop_core::attach::TextAttachment>,
}

impl ChatApp {
    /// 记下这块 Pane（同一会话最多一块：`Session::list` 只有一份）。
    pub(crate) fn register_chat_pane(&mut self, pane: WeakEntity<super::chat_pane::ChatPane>) {
        self.chat_panes.push(pane);
    }

    /// 某个会话现在有没有开着的对话 Pane
    pub(crate) fn chat_pane_of_token(
        &self,
        token: u64,
        cx: &App,
    ) -> Option<Entity<super::chat_pane::ChatPane>> {
        self.chat_panes
            .iter()
            .filter_map(|pane| pane.upgrade())
            .find(|pane| pane.read(cx).token() == token)
    }

    /// 把真输入框里的东西存回它所属的会话。
    fn stash_composer(&mut self, token: u64, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        let pending = std::mem::take(&mut self.pending);
        let pending_files = std::mem::take(&mut self.pending_files);
        self.chat_drafts.insert(token, ChatDraft { text, pending, pending_files });
    }

    /// 把某个会话的草稿装进真输入框。
    fn restore_composer(&mut self, token: u64, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.chat_drafts.remove(&token).unwrap_or(ChatDraft {
            text: String::new(),
            pending: Vec::new(),
            pending_files: Vec::new(),
        });
        self.input.update(cx, |state, cx| state.set_value(draft.text, window, cx));
        self.pending = draft.pending;
        self.pending_files = draft.pending_files;
    }

    /// 把焦点交给某一块对话 Pane：草稿热切换（旧的那块存回去，这块的装进来）。
    pub(crate) fn focus_chat(&mut self, token: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.focused_chat == Some(token) {
            return;
        }
        let previous = self.focused_chat.unwrap_or_else(|| self.session().token);
        self.stash_composer(previous, cx);
        self.focused_chat = Some(token);
        if let Some(idx) = self.index_of_token(token) {
            self.active = idx;
        }
        self.restore_composer(token, window, cx);
        cx.notify();
    }

    /// 主窗切了活跃会话（侧栏点击 / 命令面板 / 切空间）：让聚焦的那块 Pane 跟上。
    ///
    /// 目标会话已经在别的 Pane 里开着 → 聚焦那一块（**不能**让两块 Pane 画同一个
    /// `Session::list`）；否则把聚焦的那块换绑到新会话。
    pub(crate) fn sync_focus_to_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_panes.is_empty() {
            return;
        }
        let active = self.session().token;
        if self.focused_chat == Some(active) {
            return;
        }
        // 没聚焦过（启动时）：直接认活跃会话
        if self.focused_chat.is_none() {
            self.focused_chat = Some(active);
            self.content_view.update(cx, |view, cx| view.set_token(active, cx));
            return;
        }
        if self.chat_pane_of_token(active, cx).is_some() {
            // 那个会话已经在另一块 Pane 里开着：聚焦它（草稿也要热换进去）
            self.focus_chat(active, window, cx);
            return;
        }
        // 换绑聚焦的那块：`set_token` 会把它**自己的**内容区跟着换，
        // 这里不要再动 `self.content_view`（那是第一块 Pane 的，焦点在第二块时会误改它）
        if let Some(pane) = self.focused_chat.and_then(|token| self.chat_pane_of_token(token, cx)) {
            pane.update(cx, |pane, cx| pane.set_token(active, cx));
        }
        self.focused_chat = Some(active);
        if let Some(idx) = self.index_of_token(active) {
            self.active = idx;
        }
    }

    /// 侧栏 `Ctrl+点击`：在右边新开一块对话 Pane（这个会话已经开着就直接聚焦它）。
    pub(crate) fn open_chat_in_pane(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(token) = self.sessions.get(idx).map(|session| session.token) else { return };
        if self.chat_pane_of_token(token, cx).is_some() {
            self.focus_chat(token, window, cx);
            return;
        }
        // 每一块都要自己的位置池 / 内容区 / 覆盖层：共用会互相清空转圈、也会抢滚动状态
        let spots = crate::app::motion::LoopSpots::new();
        let app = cx.entity().downgrade();
        let content = cx.new(|_| ContentView::new(app.clone(), token, spots.clone()));
        let overlay = cx.new(|_| LoopOverlay::new(spots));
        let pane = cx.new(|cx| super::chat_pane::ChatPane::new(app, token, content, overlay, cx));
        self.register_chat_pane(pane.downgrade());
        // 直接和中间那块并排 Split：不受右侧文件 / 改动栏开合的影响
        // （塞进 Right 区域的话，右栏收着时新开的对话会藏在折叠栏里）
        let dock = self.dock.clone();
        let root = dock.read(cx).layout(DockPlacement::Center).map(|tree| tree.root().id());
        dock.update(cx, |dock, cx| match root {
            Some(node) => {
                dock.add_panel_view_at(
                    Arc::new(PaneHandle::new(pane)) as Arc<dyn PanelView>,
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
                    Arc::new(PaneHandle::new(pane)) as Arc<dyn PanelView>,
                    DockPlacement::Center,
                    None,
                    window,
                    cx,
                );
            }
        });
        self.focus_chat(token, window, cx);
        cx.notify();
    }

    /// 某块对话 Pane 被关掉：丢掉它的草稿、从名册里划掉，必要时把焦点交给剩下的一块。
    ///
    /// 按**实体 id** 划名册：重排布局时新旧两块可能绑同一个会话，
    /// 按令牌划会把刚建好的那块一起划掉。
    pub(crate) fn forget_chat_pane(
        &mut self,
        entity: EntityId,
        token: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_panes.retain(|pane| pane.entity_id() != entity);
        // 删草稿前确认确实没有同会话的别的 pane 了
        let still_open = self
            .chat_panes
            .iter()
            .filter_map(|pane| pane.upgrade())
            .any(|pane| pane.read(cx).token() == token);
        if !still_open {
            self.chat_drafts.remove(&token);
        }
        if self.focused_chat != Some(token) {
            cx.notify();
            return;
        }
        // 关掉的正好是挂着输入框的那块：把焦点交给剩下的任意一块
        self.focused_chat = None;
        let next = self.chat_panes.iter().filter_map(|pane| pane.upgrade()).next();
        if let Some(next) = next {
            let next_token = next.read(cx).token();
            self.focused_chat = Some(next_token);
            if let Some(idx) = self.index_of_token(next_token) {
                self.active = idx;
            }
            self.restore_composer(next_token, window, cx);
        }
        cx.notify();
    }

    /// 一块对话 Pane 的正文：聚焦的那块挂真输入框，其余画同款占位卡（点它切过来）。
    pub(crate) fn render_chat_pane_view(
        &mut self,
        token: u64,
        focused: bool,
        content: Entity<ContentView>,
        overlay: Entity<LoopOverlay>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 设置页也是一块内容区，两块 Pane 各画一份会共用 settings_scroll：只让聚焦的这块画
        if self.settings_open && !focused {
            return div().size_full().into_any_element();
        }
        let body = self.render_chat_body_with(token, focused, content, overlay, window, cx);
        div()
            .relative()
            .size_full()
            .min_h_0()
            .when(!focused, |d| {
                // 点进这块就切焦点。用**捕获阶段**：对话里的按钮（复制 / 点 Diff / 权限）
                // 很多会 `stop_propagation`，冒泡阶段的监听器永远不会先跑
                d.capture_any_mouse_down(
                    cx.listener(move |this, _, window, cx| this.focus_chat(token, window, cx)),
                )
            })
            .child(body)
            .into_any_element()
    }

    /// 非聚焦 Pane 的输入占位卡：显示它的草稿，点一下切过去。
    fn render_inactive_composer(&mut self, token: u64, cx: &mut Context<Self>) -> AnyElement {
        let draft = self
            .chat_drafts
            .get(&token)
            .map(|draft| (draft.text.clone(), draft.pending.len(), draft.pending_files.len()))
            .unwrap_or_default();
        let (text, images, files) = draft;
        let placeholder = text.trim().is_empty();
        div()
            .w_full()
            .max_w(crate::theme::READING_W)
            .px_3()
            .py_3()
            .rounded_lg()
            .border_1()
            .border_color(crate::theme::rgb(crate::theme::LINE))
            .bg(crate::theme::rgb(crate::theme::PAPER))
            .text_size(crate::theme::TEXT_BODY)
            .text_color(crate::theme::rgb(if placeholder {
                crate::theme::INK_FAINT
            } else {
                crate::theme::INK
            }))
            .cursor_text()
            .child(if placeholder {
                SharedString::from("点这里在这块对话里输入…")
            } else {
                SharedString::from(text)
            })
            .when(images + files > 0, |d| {
                d.child(
                    div()
                        .pt_1()
                        .text_size(crate::theme::TEXT_META)
                        .text_color(crate::theme::rgb(crate::theme::INK_SOFT))
                        .child(format!("附 {images} 张图 / {files} 个文件")),
                )
            })
            .capture_any_mouse_down(
                cx.listener(move |this, _, window, cx| this.focus_chat(token, window, cx)),
            )
            .into_any_element()
    }
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
                // `position` 是主窗客户区坐标；开窗要的是**屏幕**坐标，左/上拖出时
                // 不加上主窗原点，窗口会飞到屏幕外
                let origin = window.bounds().origin;
                let at = point(position.x + origin.x, position.y + origin.y);
                this.popout_pane(panel, Some(at), window, cx);
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

        // 内容区绑到当前活跃会话（多 ChatPane 之后每块各带自己的那份，见 content.rs）
        let token = self.session().token;
        self.content_view.update(cx, |view, cx| view.set_token(token, cx));

        // 上次的布局优先；读不懂 / 没存过 / 恢复出来一个没有对话 Pane 的布局，都用默认布局。
        // （没有对话 Pane 的窗口等于空壳，宁可丢一次布局）
        let Some(file) = persist::read() else {
            self.install_default_layout(window, cx);
            self.sync_right_dock(window, cx);
            return;
        };
        // 注册表重建对话面板时会读取会话、登记面板。加载主布局和独立窗时都要释放主窗借用。
        let main = cx.entity();
        window.defer(cx, move |window, cx| {
            let restored = dock.update(cx, |dock, cx| dock.load(file.main, window, cx).is_ok());
            let keep_popouts = main.update(cx, |app, cx| {
                let has_chat = restored && app.focus_pane_of_kind(PaneKind::Chat, window, cx);
                if !has_chat {
                    app.install_default_layout(window, cx);
                }
                app.sync_right_dock(window, cx);
                has_chat
            });
            if keep_popouts && !file.popouts.is_empty() {
                Self::restore_popouts(&main, file.popouts, cx);
            }
        });
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
        window: &mut Window,
        cx: &mut App,
    ) -> Arc<dyn PanelView> {
        match kind {
            PaneKind::Chat => {
                // 恢复出来的每一块对话 Pane 都建**自己的**内容区与覆盖层：
                // 共用主窗那份会让两块画同一个 `Session::list`（而且漏注册名册）
                let Some(upgraded) = app.upgrade() else {
                    return Arc::new(PaneHandle::new(cx.new(|cx| FilesPane::new(app, cx))));
                };
                let token = upgraded.read(cx).session().token;
                let spots = crate::app::motion::LoopSpots::new();
                let content = cx.new(|_| ContentView::new(app.clone(), token, spots.clone()));
                let overlay = cx.new(|_| LoopOverlay::new(spots));
                let pane = cx.new(|cx| ChatPane::new(app.clone(), token, content, overlay, cx));
                let entered = pane.clone();
                upgraded.update(cx, |app, _| {
                    app.register_chat_pane(entered.downgrade());
                });
                Arc::new(PaneHandle::new(pane))
            }
            PaneKind::Files => Arc::new(PaneHandle::new(cx.new(|cx| FilesPane::new(app, cx)))),
            PaneKind::Diff => Arc::new(PaneHandle::new(cx.new(|cx| DiffPane::new(app, cx)))),
            PaneKind::Plan => Arc::new(PaneHandle::new(cx.new(|cx| PlanPane::new(app, cx)))),
            PaneKind::Subagent => {
                Arc::new(PaneHandle::new(cx.new(|cx| SubagentPane::new(app, cx))))
            }
            // 终端：布局里存下来就重开一块（历史不恢复，进程重启后是新 shell）
            PaneKind::Terminal => Arc::new(PaneHandle::new(
                cx.new(|cx| super::terminal_pane::TerminalPane::new(app, cx)),
            )),
            // 侧问不落盘，恢复时给一块**空的新侧问**（影子会话是新建的），
            // 而不是像以前那样兜底成一块莫名其妙的文件面板
            PaneKind::SideChat => {
                let Some(upgraded) = app.upgrade() else {
                    return Arc::new(PaneHandle::new(cx.new(|cx| FilesPane::new(app, cx))));
                };
                let session = upgraded.update(cx, |app, cx| app.spawn_side_chat_session(cx));
                let pane = cx.new(|cx| {
                    super::side_chat_pane::SideChatPane::new(app.clone(), session, window, cx)
                });
                let registered = pane.clone();
                upgraded.update(cx, |app, _| {
                    app.register_side_chat(session, registered.downgrade());
                });
                Arc::new(PaneHandle::new(pane))
            }
            // 其余 Pane 在后续阶段接上；先按文件面板兜底，别让旧布局整份作废
            _ => Arc::new(PaneHandle::new(cx.new(|cx| FilesPane::new(app, cx)))),
        }
    }

    pub(crate) fn new_chat_pane(&mut self, cx: &mut Context<Self>) -> PaneHandle {
        let app = cx.entity().downgrade();
        let token = self.session().token;
        let content = self.content_view.clone();
        content.update(cx, |view, cx| view.set_token(token, cx));
        let overlay = self.loop_overlay.clone();
        let pane = cx.new(|cx| ChatPane::new(app, token, content, overlay, cx));
        self.register_chat_pane(pane.downgrade());
        // 第一块对话 Pane 默认就是焦距
        if self.focused_chat.is_none() {
            self.focused_chat = Some(token);
        }
        PaneHandle::new(pane)
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
        // (区域, 面板 id, 它在哪个窗口里；None = 主窗)
        let mut found: Option<(
            Entity<gpui_base::dock::DockArea>,
            gpui_base::dock::PanelId,
            Option<AnyWindowHandle>,
        )> = None;
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
                    found = Some((dock.clone(), id, None));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        // 弹出窗里也算「已经在工作区里」：在那个窗里选中，并把它唤到前台
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
                        found = Some((entry.dock.clone(), id, Some(entry.window)));
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
        }
        match found {
            Some((area, id, Some(popout))) => {
                // 面板在别的窗口里：选中要在**那个**窗口的上下文里做（手里这个 window 是主窗）
                let _ = popout.update(cx, |_, popout_window, cx| {
                    popout_window.activate_window();
                    area.update(cx, |dock, cx| dock.select_panel(id, popout_window, cx));
                });
                true
            }
            Some((area, id, None)) => {
                area.update(cx, |dock, cx| dock.select_panel(id, window, cx));
                true
            }
            None => false,
        }
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
            let Ok((dock, popouts)) = this.update(cx, |app, _| {
                app.layout_save = None;
                let popouts =
                    app.popouts.iter().map(|p| (p.dock.clone(), p.window)).collect::<Vec<_>>();
                (app.dock.clone(), popouts)
            }) else {
                return;
            };
            // 面板序列化时会读取主窗的文件树和差异状态，必须先结束对主窗的更新。
            cx.update(|cx| {
                let mut file = persist::snapshot(dock.read(cx), cx);
                // 弹出窗也记一笔（位置 + 内部布局），下次打开时能照原样摆回去
                for (dock, window) in popouts {
                    let area = dock.read(cx).dump(cx);
                    let bounds = window.update(cx, |_, window, _| window.bounds());
                    if let Ok(bounds) = bounds {
                        let bounds = persist::PersistedBounds {
                            x: f32::from(bounds.origin.x),
                            y: f32::from(bounds.origin.y),
                            width: f32::from(bounds.size.width),
                            height: f32::from(bounds.size.height),
                        };
                        file.popouts.push(persist::PopoutLayoutState { bounds, area });
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
    pub(crate) fn render_chat_body_with(
        &mut self,
        token: u64,
        focused: bool,
        content: Entity<ContentView>,
        overlay: Entity<LoopOverlay>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 这一段里 `self.session()` 指向这块 pane 的会话（画完还原）
        self.with_active(token, |app| {
            let composer = if focused {
                app.reading_column(div().pb_4().pt_2()).child(app.render_composer(window, cx))
            } else {
                app.reading_column(div().pb_4().pt_2())
                    .child(app.render_inactive_composer(token, cx))
            };
            let body = v_flex()
                .relative()
                .size_full()
                .min_h_0()
                .w_full()
                .when(app.home_backdrop_active(), |d| d.child(app.home_backdrop_view.clone()))
                .child(app.content_slot_with(content, overlay))
                .when(!app.settings_open, |d| d.child(composer));
            body.when(!app.settings_open, |d| app.with_file_drop(d, cx)).into_any_element()
        })
    }
}
