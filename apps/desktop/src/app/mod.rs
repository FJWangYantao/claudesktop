//! 应用根视图 `ChatApp`：会话管理、内核事件分发与整体布局。
//! 各区域的渲染拆在子模块里（都是 `impl ChatApp`）。

mod chat;
mod chrome;
mod complete_view;
mod composer;
pub(crate) mod keys;
mod menu;
mod notify;
mod overview;
mod settings_view;
mod sidebar;
mod status;
mod widgets;

use crate::demo::*;
use settings_view::SettingsForm;
use crate::model::*;
use crate::session::*;
use crate::settings::*;
use crate::theme::*;
use crate::theme::rgb;
use crate::usage::{self, TurnStat};
use claudesktop_core::{
    AcpKernel, AcpKernelConfig, EventSink, Kernel, KernelEvent, PromptImage, Record, RoutedEvent,
    SessionStore, UserPrompt,
};
use claudesktop_builtin::BuiltinKernel;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 启动时最多恢复的历史会话数（最近更新的优先）。
const MAX_RESTORED: usize = 100;

pub struct ChatApp {
    input: Entity<TextareaState>,
    /// 输入框待发送的图片附件（粘贴 / 选择文件加入，随下一条消息一起发出）
    pub(crate) pending: Vec<PendingImage>,
    /// 内核会话（可多个）；至少有一个，`active` 始终有效
    sessions: Vec<Session>,
    active: usize,
    next_token: u64,
    /// 会话持久化；None = 不落盘（演示回放模式或数据目录不可用）
    store: Option<SessionStore>,
    /// 默认工作目录（新会话用）
    cwd: PathBuf,
    /// 当前项目名（取工作目录名），侧栏分组名与输入框标签用
    project_name: SharedString,
    overview_tab: usize, // 0 概览 / 1 模型
    overview_range: usize, // 0 全部 / 1 30 天 / 2 7 天
    sidebar_open: bool,
    /// 侧栏会话按最近活动排序（否则按打开顺序）
    sessions_by_activity: bool,
    /// 侧栏里正在重命名的会话（令牌）
    renaming: Option<u64>,
    rename_input: Entity<InputState>,
    /// 重命名框真正拿到过焦点（之后失焦才算提交；刚打开时的失焦事件不算）
    rename_focused: bool,
    /// 输入框的 `/指令`、`@文件` 补全（见 `complete_view.rs`）
    completion: complete_view::CompletionState,
    /// 侧栏里等待确认删除的会话（令牌）
    confirm_delete: Option<u64>,
    /// 内核槽，按 [`KernelChoice::index`]：用到时才启动，可以同时在线，会话各自绑定一个
    kernels: [KernelSlot; 3],
    ev_tx: UnboundedSender<(KernelChoice, u64, RoutedEvent)>,
    /// 环境变量给的 ACP 基础配置（工作目录、调试开关）
    acp_base: AcpKernelConfig,
    /// 生效中的设置（启动时已合并环境变量）
    settings: Settings,
    settings_path: Option<PathBuf>,
    settings_open: bool,
    /// 设置页的焦点锚（设置页没有输入框可以兜底，快捷键靠它留在 `ChatApp` 键上下文里）
    settings_focus: FocusHandle,
    settings_form: SettingsForm,
    /// 正在后台获取模型列表
    fetching_models: bool,
    /// 本次启动已为上下文窗口大小悄悄取过一次模型列表（见 `probe_context_window`）
    context_probed: bool,
    /// 刚点了「复制」的地方（会话令牌, 记录下标, 代码块在 Markdown 里的起点；整条消息为 None），
    /// 1.5 秒内按钮显示「已复制」
    copied: Option<(u64, usize, Option<usize>)>,
    /// 所有会话的每轮统计（首页用量概览）
    usage: Vec<TurnStat>,
    /// 压测钩子：每次 render 的时刻（None = 不记录）
    pub(crate) frame_log: Option<Vec<std::time::Instant>>,
    /// 窗口在前台：激活且没最小化（不在前台时才发系统通知，见 `notify.rs`）
    window_active: bool,
    /// 发出去还没撤的系统通知标签（回到窗口时撤掉）
    notified: Vec<SharedString>,
}

/// 一种内核的运行状态。
#[derive(Default)]
pub(crate) struct KernelSlot {
    /// None = 演示回放、还没启动或启动失败
    kernel: Option<Arc<dyn Kernel>>,
    /// 启动过（失败也算；改了设置才会重试）
    started: bool,
    /// 代号：每次（重新）启动 +1，事件带着代号回来，不是当前代号的丢弃
    generation: u64,
    /// 与内核的连接已建立（可以创建会话）
    ready: bool,
    error: bool,
    status: SharedString,
    /// 内核标识（写进会话元信息，续接时比对）
    agent_cmd: String,
}

impl Drop for ChatApp {
    fn drop(&mut self) {
        for slot in &self.kernels {
            if let Some(kernel) = &slot.kernel {
                kernel.shutdown();
            }
        }
    }
}

impl ChatApp {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 多行、随内容长高（最多 10 行后出滚动条）；回车发送，Shift+回车换行
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true)
                .placeholder("描述要做的事，回车发送，Shift+回车换行")
        });

        cx.subscribe_in(
            &input,
            window,
            |this: &mut ChatApp,
             _entity: &Entity<TextareaState>,
             event: &InputEvent,
             window: &mut Window,
             cx: &mut Context<ChatApp>| {
                match event {
                    // Shift+回车已由输入框插入换行，同样会发这个事件，要排除
                    InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
                    InputEvent::PressEnter { .. } => {}
                    // 打字时刷新 `/`、`@` 补全
                    InputEvent::Change => {
                        this.update_completion(cx);
                        cx.notify();
                    }
                    // 发送按钮随输入框空 / 非空换样式；输入框卡片随聚焦换边框
                    InputEvent::Focus | InputEvent::Blur => cx.notify(),
                }
            },
        )
        .detach();
        // 快捷键挂在根视图的键上下文上，要求焦点在界面里：启动时给输入框，
        // 聚焦的元素被移出界面时（关设置页、重命名提交、菜单关闭）再找回来
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.on_focus_lost(window, |this: &mut ChatApp, window, cx| this.restore_focus(window, cx))
            .detach();
        // 外观跟随系统时，系统切深浅色要跟着变
        cx.observe_window_appearance(window, |this: &mut ChatApp, window, cx| {
            this.apply_appearance(window, cx)
        })
        .detach();


        // 内核与配置：设置文件 + 环境变量覆盖（见 settings.rs）；
        // 每次（重新）启动内核都换一个代号，旧内核的迟到事件按代号丢弃
        let acp_base = AcpKernelConfig::from_env();
        let settings_path = Settings::default_path();
        let (settings, settings_error) = Settings::load(settings_path.as_deref());
        let settings = settings.with_env();
        // 默认工作目录：环境变量 ACP_CWD 优先，其次上次选的（目录还在的话），最后是启动目录
        let cwd = settings
            .workspace
            .clone()
            .filter(|dir| std::env::var_os("ACP_CWD").is_none() && dir.is_dir())
            .unwrap_or_else(|| acp_base.cwd.clone());
        let project_name = project_label(&cwd);

        let (ev_tx, mut ev_rx) =
            futures::channel::mpsc::unbounded::<(KernelChoice, u64, RoutedEvent)>();
        cx.spawn(async move |this, cx| {
            while let Some((kind, generation, event)) = ev_rx.next().await {
                if this
                    .update(cx, |app, cx| {
                        if generation == app.kernels[kind.index()].generation {
                            app.on_kernel_event(kind, event, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // 运行状态行计时：当前会话忙时每秒重绘一次（等授权时没有转圈动画驱动重绘）
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let alive = this.update(cx, |app, cx| {
                if app.session().busy {
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();

        // 侧栏重命名框：回车或失焦即提交
        let rename_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(
            &rename_input,
            window,
            |this: &mut ChatApp, _: &Entity<InputState>, event: &InputEvent, _, cx| {
                match event {
                    InputEvent::Focus => this.rename_focused = true,
                    InputEvent::PressEnter { .. } => this.commit_rename(cx),
                    InputEvent::Blur if this.rename_focused => this.commit_rename(cx),
                    _ => {}
                }
            },
        )
        .detach();

        let settings_form = SettingsForm::new(window, cx);
        let mut app = Self {
            input,
            pending: Vec::new(),
            sessions: Vec::new(),
            active: 0,
            next_token: 1,
            store: None,
            cwd,
            project_name,
            overview_tab: 0,
            overview_range: 0,
            sidebar_open: true,
            sessions_by_activity: false,
            renaming: None,
            rename_input,
            rename_focused: false,
            completion: Default::default(),
            confirm_delete: None,
            kernels: Default::default(),
            ev_tx,
            acp_base,
            settings,
            settings_path,
            settings_open: false,
            settings_focus: cx.focus_handle(),
            settings_form,
            fetching_models: false,
            context_probed: false,
            copied: None,
            usage: Vec::new(),
            frame_log: None,
            window_active: true,
            notified: Vec::new(),
        };
        // 第一个会话用默认内核：连接就绪（Connected）后统一补发 session/new；
        // 其他内核等有会话用到时再启动
        let cwd = app.cwd.clone();
        app.open_session(app.settings.kernel(), cwd);
        if let Some(err) = settings_error {
            app.sessions[0].note(err);
        }
        // 从磁盘恢复历史会话（演示回放模式下新会话不落盘，但历史照常显示）
        match SessionStore::open_default() {
            Ok(store) => {
                // 最近的进侧栏；更早的只读进用量统计
                for (i, meta) in store.list().into_iter().enumerate() {
                    let records = store.load(&meta.id).unwrap_or_default();
                    app.usage.extend(usage::collect(&meta, &records));
                    if i < MAX_RESTORED {
                        app.sessions.push(Session::from_records(app.next_token, meta, records));
                        app.next_token += 1;
                    }
                }
                app.store = Some(store);
            }
            Err(err) => app.sessions[0].note(format!("会话历史不可用：{err}")),
        }
        app.apply_appearance(window, cx);
        app.init_notifications(window, cx);
        app
    }

    /// 按当前设置启动某种内核（不处理会话，见 [`Self::restart_kernel`]）。
    fn spawn_kernel(&mut self, kind: KernelChoice) {
        let slot = &mut self.kernels[kind.index()];
        slot.started = true;
        slot.generation += 1;
        let generation = slot.generation;
        let tx = self.ev_tx.clone();
        let sink: EventSink = Arc::new(move |event| {
            let _ = tx.unbounded_send((kind, generation, event));
        });
        let (kernel, status, agent_cmd): (Option<Arc<dyn Kernel>>, SharedString, String) =
            match kind {
                KernelChoice::Demo => (None, "未连接内核".into(), String::new()),
                KernelChoice::Builtin => match self
                    .settings
                    .builtin_config(self.cwd.clone())
                    .and_then(|config| {
                        let identity = config.identity();
                        BuiltinKernel::spawn(config, sink).map(|k| (k, identity))
                    }) {
                    Ok((k, identity)) => {
                        (Some(Arc::new(k) as Arc<dyn Kernel>), "内核启动中…".into(), identity)
                    }
                    Err(err) => (None, format!("启动失败：{err}").into(), String::new()),
                },
                KernelChoice::Acp => {
                    let config = self.settings.acp_config(self.acp_base.clone());
                    let agent_cmd = config.command.clone();
                    match AcpKernel::spawn(config, sink) {
                        Ok(k) => (Some(Arc::new(k) as Arc<dyn Kernel>), "内核启动中…".into(), agent_cmd),
                        Err(err) => (None, format!("启动失败：{err}").into(), agent_cmd),
                    }
                }
            };
        let slot = &mut self.kernels[kind.index()];
        slot.ready = kind == KernelChoice::Demo;
        slot.error = kernel.is_none() && kind != KernelChoice::Demo;
        slot.kernel = kernel;
        slot.status = status;
        slot.agent_cmd = agent_cmd;
    }

    /// 还没启动过就启动。
    fn ensure_kernel(&mut self, kind: KernelChoice) {
        if !self.kernels[kind.index()].started {
            self.spawn_kernel(kind);
        }
    }

    /// 某种内核的设置改了：关掉旧的，绑在它上面的会话全部脱离；
    /// 有对话的会话下次发消息时按「历史会话」续接，空会话等新连接就绪后重新创建。
    /// 没启动过的内核什么也不做（用到时自然按新设置启动）。
    pub(crate) fn restart_kernel(&mut self, kind: KernelChoice, cx: &mut Context<Self>) {
        if !self.kernels[kind.index()].started {
            return;
        }
        let old = self.kernels[kind.index()].kernel.take();
        for s in self.sessions.iter_mut().filter(|s| s.kernel == kind) {
            s.detach_kernel(old.as_ref());
        }
        if let Some(old) = old {
            old.shutdown();
        }
        self.spawn_kernel(kind);
        self.request_pending_sessions();
        cx.notify();
    }

    pub(crate) fn kernel_of(&self, kind: KernelChoice) -> Option<Arc<dyn Kernel>> {
        self.kernels[kind.index()].kernel.clone()
    }

    /// 第 `idx` 个会话绑定的内核
    pub(crate) fn session_kernel(&self, idx: usize) -> Option<Arc<dyn Kernel>> {
        self.kernel_of(self.sessions[idx].kernel)
    }

    pub(crate) fn session(&self) -> &Session {
        &self.sessions[self.active]
    }

    /// 新建一个会话并切过去（用默认内核）。
    pub(crate) fn add_session(&mut self, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        self.active = self.open_session(self.settings.kernel(), cwd);
        cx.notify();
    }

    /// 在某种内核上开一个会话，返回下标。内核没启动就先启动；
    /// 连接已就绪就立刻向内核要会话，否则等 Connected 时补发。
    fn open_session(&mut self, kind: KernelChoice, cwd: PathBuf) -> usize {
        self.ensure_kernel(kind);
        let token = self.next_token;
        self.next_token += 1;
        self.sessions.push(Session::new(token, kind, cwd));
        self.request_pending_sessions();
        self.sessions.len() - 1
    }

    /// 把一个还没对话的会话换到另一种内核上：原地换成新会话，
    /// 旧内核会话的迟到事件因为令牌 / 内核不同会被忽略。
    pub(crate) fn rebind_session(&mut self, idx: usize, kind: KernelChoice) {
        let s = &self.sessions[idx];
        if s.kernel == kind || s.has_user_message() || s.busy {
            return;
        }
        let cwd = s.cwd.clone();
        self.replace_session(idx, kind, cwd);
    }

    /// 第 `idx` 个会话原地换成 `kind` 内核、`cwd` 目录下的新会话（调用方保证它还没对话）。
    fn replace_session(&mut self, idx: usize, kind: KernelChoice, cwd: PathBuf) {
        let old = self.session_kernel(idx);
        self.sessions[idx].cancel_pending_permissions(old.as_ref());
        self.ensure_kernel(kind);
        let token = self.next_token;
        self.next_token += 1;
        self.sessions[idx] = Session::new(token, kind, cwd);
        self.request_pending_sessions();
    }

    /// 切换工作目录（输入框上方的目录 chip）：设为新会话的默认目录并记进设置；
    /// 当前会话还没对话就原地换到这个目录，已有对话就在那里开一个新会话（对话中途不换目录）。
    pub(crate) fn switch_workspace(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.project_name = project_label(&dir);
        self.cwd = dir.clone();
        self.update_settings(|s| s.workspace = Some(dir.clone()));
        let s = self.session();
        if s.cwd != dir {
            if s.has_user_message() || s.busy {
                self.active = self.open_session(self.settings.kernel(), dir);
            } else {
                let kind = s.kernel;
                self.replace_session(self.active, kind, dir);
            }
        }
        cx.notify();
    }

    /// 目录菜单里的「选择文件夹…」：系统对话框选一个目录切过去。
    pub(crate) fn pick_workspace(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择工作目录".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(dir) = paths.into_iter().next() {
                    let _ = this.update(cx, |app, cx| app.switch_workspace(dir, cx));
                }
            }
        })
        .detach();
    }

    /// 目录菜单列出的工作目录：默认目录在前，其余按会话最近活动排，已不存在的略去，最多 8 个。
    pub(crate) fn recent_workspaces(&self) -> Vec<PathBuf> {
        // 无元信息的会话（刚开、还没落盘）视为最新；排序稳定，默认目录排在同为最新的前面
        let mut dirs: Vec<(&Path, u64)> = vec![(self.cwd.as_path(), u64::MAX)];
        for s in &self.sessions {
            let at = s.meta.as_ref().map_or(u64::MAX, |m| m.updated_ms);
            match dirs.iter_mut().find(|(d, _)| *d == s.cwd.as_path()) {
                Some((_, last)) => *last = (*last).max(at),
                None => dirs.push((s.cwd.as_path(), at)),
            }
        }
        dirs.sort_by(|a, b| b.1.cmp(&a.1));
        dirs.into_iter()
            .map(|(d, _)| d.to_path_buf())
            .filter(|d| d.is_dir())
            .take(8)
            .collect()
    }

    /// 切到第 `i` 个会话（侧栏点击 / 快捷键共用）：重开会话的模型名从日志恢复了，
    /// 但窗口大小可能还没缓存过，顺带悄悄探测一次（见 [`Self::probe_context_window`]）。
    pub(crate) fn activate_session(&mut self, i: usize, cx: &mut Context<Self>) {
        self.active = i;
        self.settings_open = false;
        self.probe_context_window(i, cx);
        cx.notify();
    }

    /// 失败卡片上的「重试」：把第 `failed_idx` 行所在这一轮的用户消息原样再发一遍。
    pub(crate) fn retry_turn(&mut self, failed_idx: usize, cx: &mut Context<Self>) {
        let idx = self.active;
        let s = &self.sessions[idx];
        if s.busy {
            return;
        }
        let Some(ChatItem::User { text, images }) =
            s.turn_prompt(failed_idx).and_then(|u| s.transcript.get(u))
        else {
            return;
        };
        let prompt = UserPrompt {
            text: text.to_string(),
            images: images.iter().filter_map(|p| load_prompt_image(p)).collect(),
        };
        self.submit_to(idx, prompt, cx);
    }

    /// 同一任务换个内核跑：拿当前会话里第 `item_idx` 条用户消息，在 `kind` 上新开会话发出去。
    pub(crate) fn rerun_on(&mut self, item_idx: usize, kind: KernelChoice, cx: &mut Context<Self>) {
        let src = self.active;
        let Some(ChatItem::User { text, images }) = self.sessions[src].transcript.get(item_idx)
        else {
            return;
        };
        let prompt = UserPrompt {
            text: text.to_string(),
            images: images.iter().filter_map(|p| load_prompt_image(p)).collect(),
        };
        let from = self.sessions[src].title.clone();
        let cwd = self.sessions[src].cwd.clone();
        let idx = self.open_session(kind, cwd);
        self.active = idx;
        self.sessions[idx].note(format!("同一任务换到「{}」上跑（原会话：{from}）", kind.label()));
        // 内核不可用时 submit_to 会给出提示
        if self.sessions[idx].ready || self.kernel_of(kind).is_none() {
            self.submit_to(idx, prompt, cx);
        } else {
            self.sessions[idx].autoprompt = Some(prompt);
        }
        cx.notify();
    }

    /// 压测钩子：往当前会话灌 `n` 行混合内容（用户消息 / markdown 回复 / 工具行 / 备注）。
    pub(crate) fn debug_stress(&mut self, n: usize, cx: &mut Context<Self>) {
        let s = &mut self.sessions[self.active];
        for i in 0..n {
            s.transcript.push(match i % 4 {
                0 => ChatItem::User {
                    text: format!("第 {i} 条：帮我看看这段代码为什么慢").into(),
                    images: Vec::new(),
                },
                1 => ChatItem::Agent(format!(
                    "**第 {i} 条回复。** 先看热点：\n\n- 每帧重建整棵树\n- 列表没有虚拟化\n\n```rust\nfn hot(i: usize) -> usize {{ i * {i} }}\n```\n\n改完再测一次。"
                )),
                2 => ChatItem::Tool(ToolCard {
                    id: None,
                    kind: "read".into(),
                    name: "Read".into(),
                    arg: format!("src/module_{i}.rs").into(),
                    result: "120 行".into(),
                    output: SharedString::default(),
                    phase: ToolPhase::Done,
                    diffs: Vec::new(),
                    expanded: false,
                }),
                _ => ChatItem::Note(format!("备注 {i}").into()),
            });
        }
        s.title = format!("压测 {n} 行").into();
        cx.notify();
    }

    /// 压测钩子：当前会话的列表滚动一段距离（正数向下）。
    pub(crate) fn debug_scroll(&mut self, dy: f32, cx: &mut Context<Self>) {
        self.sessions[self.active].list.scroll_by(px(dy));
        cx.notify();
    }

    /// 调试钩子：把当前会话的首条提示词放到 `kind` 上重跑。
    pub(crate) fn rerun_first_prompt(&mut self, kind: KernelChoice, cx: &mut Context<Self>) {
        let first = self.sessions[self.active]
            .transcript
            .iter()
            .position(|i| matches!(i, ChatItem::User { .. }));
        if let Some(item) = first {
            self.rerun_on(item, kind, cx);
        }
    }

    /// 向内核要还没要过的会话（`session/new`），以及要续接的历史会话。连接未就绪的内核先跳过。
    pub(crate) fn request_pending_sessions(&mut self) {
        for idx in 0..self.sessions.len() {
            let slot = &self.kernels[self.sessions[idx].kernel.index()];
            let Some(kernel) = slot.kernel.clone().filter(|_| slot.ready) else {
                continue;
            };
            let s = &mut self.sessions[idx];
            if s.requested {
                continue;
            }
            if s.resuming {
                self.send_resume(idx, &kernel);
            } else if !s.restored && s.acp_id.is_none() {
                s.requested = true;
                if let Err(err) = kernel.new_session(s.token, Some(s.cwd.clone())) {
                    s.requested = false;
                    s.note(format!("创建会话失败：{err}"));
                }
            }
        }
    }

    /// 历史会话续接内核会话，就绪后再发待发的消息。同一个 ACP agent 才尝试；
    /// 自研内核用本地日志重建上下文，换了端点 / 模型也能续接。
    fn send_resume(&mut self, idx: usize, kernel: &Arc<dyn Kernel>) {
        let s = &mut self.sessions[idx];
        let agent_cmd = &self.kernels[s.kernel.index()].agent_cmd;
        s.requested = true;
        let builtin = s.kernel == KernelChoice::Builtin;
        let resumable = s.meta.as_ref().filter(|m| {
            if builtin {
                m.agent.starts_with("builtin:")
            } else {
                m.agent == *agent_cmd && m.kernel_session_id.is_some()
            }
        });
        let cwd = Some(s.cwd.clone());
        let sent = match resumable {
            Some(meta) => {
                let history = match &self.store {
                    Some(store) if builtin => store.load(&meta.id).unwrap_or_default(),
                    _ => Vec::new(),
                };
                let previous = meta.kernel_session_id.clone().unwrap_or_default();
                kernel.resume_session(s.token, &previous, cwd, &history)
            }
            None => kernel.new_session(s.token, cwd),
        };
        if let Err(err) = sent {
            s.requested = false;
            s.resuming = false;
            s.restored = true;
            s.autoprompt = None;
            s.note(format!("续接会话失败：{err}"));
        }
    }

    /// 调试钩子：CLAUDESKTOP_AUTODEMO=1 时自动发一条提示词
    /// （有内核则等会话就绪后经内核发送，否则播放演示脚本）。
    pub(crate) fn autodemo_hook(&mut self, cx: &mut Context<Self>) {
        if std::env::var("CLAUDESKTOP_AUTODEMO").ok().as_deref() != Some("1") {
            return;
        }
        // CLAUDESKTOP_AUTOPROMPT 指定提示词；未指定时只在不连真实 agent
        // （ACP_AGENT 未设置）时发默认演示词，避免对真实 agent 误发写文件任务。
        let text = match std::env::var("CLAUDESKTOP_AUTOPROMPT") {
            Ok(text) if !text.trim().is_empty() => text,
            _ if std::env::var_os("ACP_AGENT").is_none() => {
                "帮我配置一个有图形界面的动态壁纸软件".to_string()
            }
            _ => return,
        };
        // CLAUDESKTOP_AUTODEMO_SESSION 指定发往第几个会话（默认 0；历史会话从 1 起）
        let idx = std::env::var("CLAUDESKTOP_AUTODEMO_SESSION")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|i| *i < self.sessions.len())
            .unwrap_or(0);
        self.active = idx;
        // 历史会话直接走 submit_to：由它先续接内核会话
        if self.sessions[idx].ready || self.sessions[idx].restored {
            self.submit_to(idx, UserPrompt::text(text), cx);
        } else {
            self.sessions[idx].autoprompt = Some(UserPrompt::text(text));
        }
        cx.notify();
    }

    pub(crate) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session().busy {
            return;
        }
        let text = self.input.read(cx).value().to_string();
        let images = self
            .pending
            .iter()
            .map(|p| PromptImage {
                media_type: p.media_type.clone(),
                data: p.data.clone(),
            })
            .collect::<Vec<_>>();
        if text.trim().is_empty() && images.is_empty() {
            return;
        }
        self.input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.pending.clear();
        self.submit_to(self.active, UserPrompt { text, images }, cx);
    }

    // ---------- 图片附件 ----------

    /// 待发送图片张数上限（多了模型端也吃不下，界面先挡住）。
    const MAX_PENDING_IMAGES: usize = 8;

    /// 加入待发送图片（超出上限截断并提示）。
    pub(crate) fn add_pending_images(
        &mut self,
        images: Vec<PendingImage>,
        cx: &mut Context<Self>,
    ) {
        let room = Self::MAX_PENDING_IMAGES.saturating_sub(self.pending.len());
        if images.is_empty() {
            return;
        }
        if images.len() > room {
            self.sessions[self.active].note(format!(
                "一轮最多附 {} 张图片，多出的没有加上",
                Self::MAX_PENDING_IMAGES
            ));
        }
        self.pending.extend(images.into_iter().take(room));
        cx.notify();
    }

    /// 撤掉第 `i` 张待发送图片。
    pub(crate) fn remove_pending_image(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.pending.len() {
            self.pending.remove(i);
            cx.notify();
        }
    }

    /// 把附件图片写进会话的资产目录，返回各自的绝对路径（仅文本消息返回空）。
    /// 演示回放没有会话存储，跳过（消息仍发送，只是不带图）。
    fn store_images(&mut self, idx: usize, images: &[PromptImage]) -> Vec<String> {
        use base64::Engine as _;
        if images.is_empty() || self.session_kernel(idx).is_none() {
            return Vec::new();
        }
        self.ensure_log(idx);
        let Some(store) = &self.store else {
            return Vec::new();
        };
        let Some(meta) = self.sessions[idx].meta.clone() else {
            return Vec::new();
        };
        images
            .iter()
            .filter_map(|image| {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(image.data.as_bytes())
                    .ok()?;
                let ext = ext_of(&image.media_type);
                store.save_image(&meta.id, ext, &bytes).ok().map(|p| {
                    p.to_string_lossy()
                        .into_owned()
                })
            })
            .collect()
    }

    /// 向第 `idx` 个会话发一轮用户消息：有内核走内核，演示回放会话播放演示脚本。
    pub(crate) fn submit_to(&mut self, idx: usize, prompt: UserPrompt, cx: &mut Context<Self>) {
        let kind = self.sessions[idx].kernel;
        if self.sessions[idx].busy {
            return;
        }
        if kind != KernelChoice::Demo {
            // 历史会话的内核可能还没启动
            self.ensure_kernel(kind);
            if self.kernel_of(kind).is_none() {
                let status = self.kernels[kind.index()].status.clone();
                self.sessions[idx].note(format!("{}不可用：{status}（可在设置里修改）", kind.label()));
                cx.notify();
                return;
            }
        }
        let kernel = self.session_kernel(idx);
        let mut start_demo_token = None;
        {
            let s = &mut self.sessions[idx];
            if kernel.is_some() && s.restored {
                // 历史会话：先续接内核会话（连接就绪后发出），就绪后再发这条消息
                s.restored = false;
                s.resuming = true;
                s.autoprompt = Some(prompt);
                s.note("正在续接会话…");
                self.request_pending_sessions();
                cx.notify();
                return;
            }
            if kernel.is_some() && !s.ready {
                s.note("会话尚未就绪，请稍候再发送");
                cx.notify();
                return;
            }
            if !s.has_user_message() && !s.renamed {
                s.title = crate::model::title_from_prompt(&prompt.text).into();
            }
            // 图片附件先落盘（需要会话元信息），路径进记录与气泡
            let stored = self.store_images(idx, &prompt.images);
            if !prompt.images.is_empty() && stored.is_empty() && kind == KernelChoice::Demo {
                self.sessions[idx].note("演示回放不保存图片附件，本轮只发送文字");
            }
            let image_paths = stored.iter().map(PathBuf::from).collect::<Vec<_>>();
            self.sessions[idx].transcript.push(ChatItem::User {
                text: SharedString::from(prompt.text.clone()),
                images: image_paths,
            });
            if kernel.is_some() {
                self.persist(
                    idx,
                    Record::User {
                        text: prompt.text.clone(),
                        images: stored,
                    },
                );
            }
            let s = &mut self.sessions[idx];
            s.begin_turn();
            s.follow_tail();
            match (&kernel, s.acp_id.clone()) {
                (Some(kernel), Some(id)) => {
                    if let Err(err) = kernel.send_prompt(&id, prompt) {
                        s.busy = false;
                        s.fail(format!("发送失败：{err}"));
                    }
                }
                (Some(_), None) => {
                    s.busy = false;
                    s.note("会话尚未就绪，请稍候再发送");
                }
                (None, _) => {
                    s.cancel_token += 1;
                    start_demo_token = Some(s.cancel_token);
                }
            }
        }
        if let Some(token) = start_demo_token {
            self.start_demo(idx, token, cx);
        }
        cx.notify();
    }

    /// 应答当前会话里的某条权限请求。
    pub(crate) fn resolve_permission(
        &mut self,
        item_idx: usize,
        choice: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let active = self.active;
        let kernel = self.session_kernel(active);
        self.sessions[active].resolve_permission(item_idx, choice, kernel.as_ref());
        if let Some(ChatItem::Permission {
            id,
            resolved: Some(label),
            ..
        }) = self.sessions[active].transcript.get(item_idx)
        {
            let record = Record::PermissionResolved {
                id: *id,
                label: label.to_string(),
            };
            self.persist(active, record);
        }
        cx.notify();
    }

    /// 手动压缩当前会话的上下文（自研内核、空闲时）：自成一轮，进度和结果作为一行出现在对话里。
    pub(crate) fn compact_session(&mut self, cx: &mut Context<Self>) {
        let active = self.active;
        let kernel = self.session_kernel(active);
        let s = &mut self.sessions[active];
        if s.busy {
            return;
        }
        match (kernel, s.acp_id.clone()) {
            (Some(kernel), Some(id)) => match kernel.compact(&id) {
                Ok(()) => {
                    s.begin_turn();
                    s.compact_turn = true;
                }
                Err(err) => s.note(format!("压缩上下文失败：{err}")),
            },
            _ => s.note("这个会话还没接上内核：先发一条消息，再压缩"),
        }
        cx.notify();
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        let active = self.active;
        let kernel = self.session_kernel(active);
        let s = &mut self.sessions[active];
        s.cancel_token += 1;
        s.busy = false;
        // 正在流式的思考块收起，不然停下后还挂着「思考中…」；压缩到一半的那行也收尾
        s.finish_thinking();
        s.settle_compaction(CompactState::Failed("已停止".into()));
        let thought = s.take_thought_done();
        s.cancel_pending_permissions(kernel.as_ref());
        match (&kernel, s.acp_id.clone()) {
            (Some(kernel), Some(id)) => {
                let _ = kernel.cancel(&id);
                s.note("已请求停止");
            }
            _ => s.note("已停止（演示回放）"),
        }
        // 演示回放不落盘
        if let (Some(ms), Some(_)) = (thought, &kernel) {
            self.persist(active, Record::Thought { elapsed_ms: ms });
        }
        cx.notify();
    }

    /// 内核事件分发：连接级事件自己处理，会话级事件在绑定这个内核的会话里按 sessionId / token 找。
    pub(crate) fn on_kernel_event(
        &mut self,
        kind: KernelChoice,
        routed: RoutedEvent,
        cx: &mut Context<Self>,
    ) {
        let RoutedEvent { session, event } = routed;
        let kernel = self.kernel_of(kind);
        let slot = &mut self.kernels[kind.index()];
        match event {
            KernelEvent::Connected { agent_name } => {
                slot.ready = true;
                slot.error = false;
                // 去掉 npm 作用域前缀（@scope/name → name），底栏空间有限
                let short = agent_name.rsplit('/').next().unwrap_or(&agent_name);
                slot.status = format!("{short} · 已连接").into();
                self.request_pending_sessions();
            }
            KernelEvent::SessionFailed { token, message } => {
                if let Some(s) = self
                    .sessions
                    .iter_mut()
                    .find(|s| s.kernel == kind && s.token == token)
                {
                    s.requested = false;
                    s.resuming = false;
                    s.note(format!("创建会话失败：{message}"));
                }
            }
            KernelEvent::Fatal(message) => {
                slot.ready = false;
                slot.error = true;
                slot.status = "内核错误".into();
                for s in self.sessions.iter_mut().filter(|s| s.kernel == kind) {
                    s.on_fatal(&message, kernel.as_ref());
                }
            }
            event => {
                // 会话级事件：Started 按令牌认领，其余按 sessionId
                let idx = match &event {
                    KernelEvent::Started { token, .. } => self
                        .sessions
                        .iter()
                        .position(|s| s.kernel == kind && s.token == *token),
                    _ => session.as_deref().and_then(|id| {
                        self.sessions
                            .iter()
                            .position(|s| s.kernel == kind && s.acp_id.as_deref() == Some(id))
                    }),
                };
                if let Some(idx) = idx {
                    let turn_over = matches!(
                        event,
                        KernelEvent::TurnEnded { .. } | KernelEvent::PromptFailed { .. }
                    );
                    // 先看（record_turn 会清掉手动压缩的标记）
                    let attention = self.sessions[idx].attention(&event);
                    if Record::worth_keeping(&event) {
                        self.persist(idx, Record::Event { event: event.clone() });
                    }
                    if turn_over {
                        self.record_turn(idx);
                        self.touch_meta(idx);
                    }
                    let prompt = self.sessions[idx].apply_event(event, kernel.as_ref());
                    // 这条事件收尾了一段思考：用时紧跟着写进日志，重放时挂回那段思考
                    if let Some(ms) = self.sessions[idx].take_thought_done() {
                        self.persist(idx, Record::Thought { elapsed_ms: ms });
                    }
                    if turn_over {
                        self.probe_context_window(idx, cx);
                    }
                    if let Some(body) = attention {
                        self.notify_attention(idx, body, cx);
                    }
                    if let Some(prompt) = prompt {
                        self.submit_to(idx, prompt, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// 确保第 `idx` 个会话的日志已打开（首次写入时建档）；失败时提示并停用存储。
    fn ensure_log(&mut self, idx: usize) {
        if self.sessions[idx].log.is_some() || self.store.is_none() {
            return;
        }
        let s = &mut self.sessions[idx];
        let agent_cmd = self.kernels[s.kernel.index()].agent_cmd.clone();
        let opened = match &s.meta {
            Some(meta) => self.store.as_ref().unwrap().append_log(&meta.id),
            None => self
                .store
                .as_ref()
                .unwrap()
                .create(&s.title, s.cwd.clone(), &agent_cmd)
                .map(|(mut meta, log)| {
                    meta.kernel_session_id = s.acp_id.clone();
                    let _ = self.store.as_ref().unwrap().save_meta(&meta);
                    s.meta = Some(meta);
                    log
                }),
        };
        match opened {
            Ok(log) => s.log = Some(log),
            Err(err) => {
                s.note(format!("会话未能保存：{err}"));
                self.store = None;
            }
        }
    }

    /// 往第 `idx` 个会话的日志追加一条记录；首次写入时建档。
    /// 写盘失败只提示一次，不影响对话本身。
    fn persist(&mut self, idx: usize, record: Record) {
        if self.store.is_none() {
            return;
        }
        self.ensure_log(idx);
        if let Some(log) = &mut self.sessions[idx].log {
            let _ = log.append(record);
        }
    }

    /// 一轮结束：把时间、模型、token、用时记进日志和用量统计。
    fn record_turn(&mut self, idx: usize) {
        let s = &mut self.sessions[idx];
        // 手动压缩自成一轮但没有用户消息：不记（用量概览只数跟在用户消息后面的轮次），
        // 也不能挂到上一轮名下，把那一轮回复下面的用时和用量换掉
        if std::mem::take(&mut s.compact_turn) {
            s.turn_started = None;
            s.turn_usage = Default::default();
            return;
        }
        let summary = s.close_turn();
        let TurnUsage {
            input: input_tokens,
            output: output_tokens,
            ..
        } = summary.usage;
        let model = Some(s.model_label.to_string()).filter(|m| m != UNKNOWN_MODEL);
        let context_tokens = Some(s.context_tokens).filter(|n| *n > 0);
        let at_ms = now_ms();
        self.persist(
            idx,
            Record::Turn {
                at_ms,
                model: model.clone(),
                input_tokens,
                output_tokens,
                elapsed_ms: summary.elapsed_ms,
                context_tokens,
            },
        );
        let s = &self.sessions[idx];
        let session = s
            .meta
            .as_ref()
            .map_or_else(|| format!("live-{}", s.token), |m| m.id.clone());
        self.usage.push(TurnStat {
            session,
            at_ms,
            model,
            tokens: input_tokens + output_tokens,
        });
    }

    /// 一轮结束：刷新日志，更新元信息（标题 / 内核会话 ID / 更新时间）。
    fn touch_meta(&mut self, idx: usize) {
        let Some(store) = &self.store else {
            return;
        };
        let s = &mut self.sessions[idx];
        if let Some(log) = &mut s.log {
            let _ = log.flush();
        }
        if let Some(meta) = &mut s.meta {
            // 续接到别的 agent 后，会话就归新的 agent 了
            meta.agent = self.kernels[s.kernel.index()].agent_cmd.clone();
            meta.title = s.title.to_string();
            meta.kernel_session_id = s.acp_id.clone();
            meta.updated_ms = now_ms();
            let _ = store.save_meta(meta);
        }
    }

    pub(crate) fn start_demo(&mut self, idx: usize, token: u64, cx: &mut Context<Self>) {
        let script = build_demo_script();
        // 按会话令牌找（中途删了别的会话，下标会变）
        let session = self.sessions[idx].token;
        cx.spawn(async move |this, cx| {
            for step in script {
                cx.background_executor()
                    .timer(Duration::from_millis(step.0))
                    .await;
                let mut cancelled = false;
                let _ = this.update(cx, |app, cx| {
                    match app.sessions.iter_mut().find(|s| s.token == session) {
                        Some(s) if s.cancel_token == token => s.apply_mock(&step.1),
                        _ => {
                            cancelled = true;
                            return;
                        }
                    }
                    cx.notify();
                });
                if cancelled {
                    return;
                }
            }
            let _ = this.update(cx, |app, cx| {
                if let Some(s) = app.sessions.iter_mut().find(|s| s.token == session) {
                    s.busy = false;
                    s.close_turn();
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ChatApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        static FIRST_FRAME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !FIRST_FRAME.swap(true, std::sync::atomic::Ordering::Relaxed) {
            if let Some(t0) = crate::STARTUP_T0.get() {
                if std::env::var("CLAUDESKTOP_BENCH_STARTUP").is_ok() {
                    let elapsed = t0.elapsed();
                    eprintln!("[startup_bench] main -> first_frame:     {:>6.2} ms", elapsed.as_secs_f64() * 1000.0);
                    if std::env::var("CLAUDESKTOP_BENCH_EXIT").is_ok() {
                        std::process::exit(0);
                    }
                }
            }
        }
        if let Some(log) = &mut self.frame_log {
            log.push(std::time::Instant::now());
        }

        let active = self.active;
        // 还没发过消息的会话显示首页（问候 + 概览）
        let home = !self.sessions[active].has_user_message();

        let composer_area = div().px_6().pb_4().pt_2().child(self.render_composer(window, cx));

        let body = if self.settings_open {
            v_flex().flex_1().min_h_0().child(self.render_settings(cx))
        } else if home {
            // 首页：问候 + 用量概览居中，输入框在底部（与官方空会话一致）
            v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("home-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(div().h_full().v_flex().child(self.render_overview(cx))),
                )
                .child(composer_area)
        } else {
            // 虚拟列表：只布局 / 绘制视口附近的行；最后一行是运行状态行（见 Session::sync_list）
            let view = self.settings.view_mode;
            self.sessions[active].sync_list(view);
            let state = self.sessions[active].list.clone();
            let scroll = list(
                state,
                cx.processor(|this, ix: usize, _window, cx| {
                    let s = &this.sessions[this.active];
                    let n = s.transcript.len();
                    let row = div().max_w(READING_W).mx_auto().w_full().px_6();
                    let content = match (s.transcript.get(ix), s.layout.get(ix)) {
                        // 收起的组内步骤、当前视图不显示的行：零高度（分组结果在测量指纹里，变了就重测）
                        (Some(_), Some(layout)) if !layout.body && layout.head.is_none() => div(),
                        (Some(item), Some(&layout)) => row
                            .pt(px(if ix == 0 { 16. } else { 0. }))
                            .child(this.render_row(ix, item, layout, cx)),
                        (None, _) if ix == n => row.pb_6().children(this.render_turn_status()),
                        _ => row,
                    };
                    div().w_full().child(content).into_any_element()
                }),
            )
            .flex_1()
            .min_h_0();

            // 上下边缘渐隐：内容滚到顶栏 / 输入区下面时淡出，不是一刀切。高度不超过首行上边距和
            // 状态行下边距，滚到头时不遮内容；覆盖层不挂事件，不挡点击和滚轮
            let paper = rgb(PAPER);
            let clear = Rgba { a: 0., ..paper };
            let fade = |h: Pixels, from: Rgba, to: Rgba| {
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .h(h)
                    .bg(linear_gradient(180., linear_color_stop(from, 0.), linear_color_stop(to, 1.)))
            };
            let transcript = div()
                .relative()
                .flex_1()
                .min_h_0()
                .v_flex()
                .child(scroll)
                .child(fade(px(16.), paper, clear).top_0())
                .child(fade(px(24.), clear, paper).bottom_0())
                .children(self.render_jump_to_bottom(cx));

            v_flex().flex_1().min_h_0().child(transcript).child(composer_area)
        };

        let main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_header(window, cx))
            .child(body);

        let mut root = div();
        // 中文回退字体：根上设一次，子元素换 font_family 时继承
        root.text_style().font_fallbacks = Some(cjk_fallbacks());
        // 不用系统标题栏：侧栏顶到窗口顶，会话区顶栏兼做拖拽区与窗口按钮（见 chrome.rs）
        let root = root
            .h_flex()
            .size_full()
            .font_family(SANS)
            .text_size(TEXT_LARGE)
            .bg(rgb(PAPER))
            .text_color(rgb(INK))
            .when(self.sidebar_open, |d| d.child(self.render_sidebar(cx)))
            .child(main);
        // 快捷键（见 keys.rs）
        self.with_key_actions(root, cx)
    }
}

/// 工作目录的显示名：取目录名，没有（如盘符根目录）就用整个路径。
pub(crate) fn project_label(dir: &Path) -> SharedString {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
        .into()
}

/// 菜单里显示的完整路径：用户目录缩写成 `~`，免得长路径撑出面板。
pub(crate) fn short_path(dir: &Path) -> String {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    match home.and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => dir.display().to_string(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// MIME 类型 → 存盘扩展名。
fn ext_of(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        _ => "png",
    }
}

/// 扩展名 → MIME 类型。
pub(crate) fn mime_of(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        _ => "image/png",
    }
}

/// 从磁盘读一张附件图片 → 发给内核的 base64（文件丢了返回 None，跳过）。
fn load_prompt_image(path: &std::path::Path) -> Option<PromptImage> {
    use base64::Engine as _;
    let bytes = std::fs::read(path).ok()?;
    Some(PromptImage {
        media_type: mime_of(path).to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}
