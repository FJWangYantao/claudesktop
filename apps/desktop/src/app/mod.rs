//! 应用根视图 `ChatApp`：会话管理、内核事件分发与整体布局。
//! 各区域的渲染拆在子模块里（都是 `impl ChatApp`）。

mod chat;
mod composer;
mod overview;
mod settings_view;
mod sidebar;
mod widgets;

use crate::demo::*;
use settings_view::SettingsForm;
use crate::model::*;
use crate::session::*;
use crate::settings::*;
use crate::theme::*;
use crate::usage::{self, TurnStat};
use claudesktop_core::{
    AcpKernel, AcpKernelConfig, EventSink, Kernel, KernelEvent, Record, RoutedEvent, SessionStore,
};
use claudesktop_builtin::BuiltinKernel;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 启动时最多恢复的历史会话数（最近更新的优先）。
const MAX_RESTORED: usize = 100;

pub struct ChatApp {
    input: Entity<InputState>,
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
    mode_menu_open: bool,
    model_menu_open: bool,
    /// 模型下拉从顶栏打开（否则锚在输入框）
    model_menu_top: bool,
    kernel_menu_open: bool,
    /// 侧栏里正在重命名的会话（令牌）
    renaming: Option<u64>,
    rename_input: Entity<InputState>,
    /// 重命名框真正拿到过焦点（之后失焦才算提交；刚打开时的失焦事件不算）
    rename_focused: bool,
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
    settings_form: SettingsForm,
    /// 正在后台获取模型列表
    fetching_models: bool,
    /// 所有会话的每轮统计（首页用量概览）
    usage: Vec<TurnStat>,
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
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("How can I help you today?")
        });

        cx.subscribe_in(
            &input,
            window,
            |this: &mut ChatApp,
             _entity: &Entity<InputState>,
             event: &InputEvent,
             window: &mut Window,
             cx: &mut Context<ChatApp>| {
                if let InputEvent::PressEnter { .. } = event {
                    this.send(window, cx);
                }
            },
        )
        .detach();


        // 内核与配置：设置文件 + 环境变量覆盖（见 settings.rs）；
        // 每次（重新）启动内核都换一个代号，旧内核的迟到事件按代号丢弃
        let acp_base = AcpKernelConfig::from_env();
        let cwd = acp_base.cwd.clone();
        let project_name: SharedString = cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "claudesktop".into())
            .into();
        let settings_path = Settings::default_path();
        let (settings, settings_error) = Settings::load(settings_path.as_deref());
        let settings = settings.with_env();

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
            sessions: Vec::new(),
            active: 0,
            next_token: 1,
            store: None,
            cwd,
            project_name,
            overview_tab: 0,
            overview_range: 0,
            sidebar_open: true,
            mode_menu_open: false,
            model_menu_open: false,
            model_menu_top: false,
            kernel_menu_open: false,
            renaming: None,
            rename_input,
            rename_focused: false,
            confirm_delete: None,
            kernels: Default::default(),
            ev_tx,
            acp_base,
            settings,
            settings_path,
            settings_open: false,
            settings_form,
            fetching_models: false,
            usage: Vec::new(),
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
                KernelChoice::Demo => (None, "演示回放（未连接内核）".into(), String::new()),
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
        let old = self.session_kernel(idx);
        self.sessions[idx].cancel_pending_permissions(old.as_ref());
        self.ensure_kernel(kind);
        let token = self.next_token;
        self.next_token += 1;
        let cwd = self.sessions[idx].cwd.clone();
        self.sessions[idx] = Session::new(token, kind, cwd);
        self.request_pending_sessions();
    }

    /// 同一任务换个内核跑：拿当前会话里第 `item_idx` 条用户消息，在 `kind` 上新开会话发出去。
    pub(crate) fn rerun_on(&mut self, item_idx: usize, kind: KernelChoice, cx: &mut Context<Self>) {
        let src = self.active;
        let Some(ChatItem::User(text)) = self.sessions[src].transcript.get(item_idx) else {
            return;
        };
        let text = text.to_string();
        let from = self.sessions[src].title.clone();
        let cwd = self.sessions[src].cwd.clone();
        let idx = self.open_session(kind, cwd);
        self.active = idx;
        self.mode_menu_open = false;
        self.model_menu_open = false;
        self.sessions[idx].note(format!("同一任务换到「{}」上跑（原会话：{from}）", kind.label()));
        // 内核不可用时 submit_to 会给出提示
        if self.sessions[idx].ready || self.kernel_of(kind).is_none() {
            self.submit_to(idx, text, cx);
        } else {
            self.sessions[idx].autoprompt = Some(text);
        }
        cx.notify();
    }

    /// 调试钩子：打开内核弹层。
    pub(crate) fn open_kernel_menu(&mut self, cx: &mut Context<Self>) {
        self.kernel_menu_open = true;
        self.mode_menu_open = false;
        self.model_menu_open = false;
        cx.notify();
    }

    /// 调试钩子：把当前会话的首条提示词放到 `kind` 上重跑。
    pub(crate) fn rerun_first_prompt(&mut self, kind: KernelChoice, cx: &mut Context<Self>) {
        let first = self.sessions[self.active]
            .transcript
            .iter()
            .position(|i| matches!(i, ChatItem::User(_)));
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
            self.submit_to(idx, text, cx);
        } else {
            self.sessions[idx].autoprompt = Some(text);
        }
        cx.notify();
    }

    pub(crate) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session().busy {
            return;
        }
        let text = self.input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.submit_to(self.active, text, cx);
    }

    /// 向第 `idx` 个会话发一轮用户消息：有内核走内核，演示回放会话播放演示脚本。
    pub(crate) fn submit_to(&mut self, idx: usize, text: String, cx: &mut Context<Self>) {
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
                s.autoprompt = Some(text);
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
                // 会话标题取首条消息的前 18 个字符
                let title: String = text.chars().take(18).collect();
                s.title = title.into();
            }
            s.transcript
                .push(ChatItem::User(SharedString::from(text.clone())));
            if kernel.is_some() {
                self.persist(idx, Record::User { text: text.clone() });
            }
            let s = &mut self.sessions[idx];
            s.busy = true;
            s.follow_scroll = true;
            match (&kernel, s.acp_id.clone()) {
                (Some(kernel), Some(id)) => {
                    if let Err(err) = kernel.send_prompt(&id, text) {
                        s.busy = false;
                        s.note(format!("发送失败：{err}"));
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

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        let kernel = self.session_kernel(self.active);
        let s = &mut self.sessions[self.active];
        s.cancel_token += 1;
        s.busy = false;
        s.cancel_pending_permissions(kernel.as_ref());
        match (&kernel, s.acp_id.clone()) {
            (Some(kernel), Some(id)) => {
                let _ = kernel.cancel(&id);
                s.note("已请求停止");
            }
            _ => s.note("已停止（演示回放）"),
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
                    if Record::worth_keeping(&event) {
                        self.persist(idx, Record::Event { event: event.clone() });
                    }
                    if turn_over {
                        self.record_turn(idx);
                        self.touch_meta(idx);
                    }
                    if let Some(text) = self.sessions[idx].apply_event(event, kernel.as_ref()) {
                        self.submit_to(idx, text, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// 往第 `idx` 个会话的日志追加一条记录；首次写入时建档。
    /// 写盘失败只提示一次，不影响对话本身。
    fn persist(&mut self, idx: usize, record: Record) {
        let Some(store) = &self.store else {
            return;
        };
        let s = &mut self.sessions[idx];
        let agent_cmd = &self.kernels[s.kernel.index()].agent_cmd;
        if s.log.is_none() {
            let opened = match &s.meta {
                Some(meta) => store.append_log(&meta.id),
                None => store
                    .create(&s.title, s.cwd.clone(), agent_cmd)
                    .map(|(mut meta, log)| {
                        meta.kernel_session_id = s.acp_id.clone();
                        let _ = store.save_meta(&meta);
                        s.meta = Some(meta);
                        log
                    }),
            };
            match opened {
                Ok(log) => s.log = Some(log),
                Err(err) => {
                    s.note(format!("会话未能保存：{err}"));
                    self.store = None;
                    return;
                }
            }
        }
        if let Some(log) = &mut s.log {
            let _ = log.append(record);
        }
    }

    /// 一轮结束：把时间、模型、token 记进日志和用量统计。
    fn record_turn(&mut self, idx: usize) {
        let s = &mut self.sessions[idx];
        let (input_tokens, output_tokens) = std::mem::take(&mut s.turn_usage);
        let model = Some(s.model_label.to_string()).filter(|m| m != UNKNOWN_MODEL);
        let at_ms = now_ms();
        self.persist(
            idx,
            Record::Turn {
                at_ms,
                model: model.clone(),
                input_tokens,
                output_tokens,
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
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ChatApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title_bar = TitleBar::new()
            .bg(rgb(SAND))
            .child(
                div()
                    .h_flex()
                    .ml_2()
                    .gap_3()
                    .items_center()
                    .child(Icon::new(IconAll::PanelLeft).size_4().text_color(rgb(INK_SOFT)))
                    .child(Icon::new(IconAll::ArrowLeft).size_4().text_color(rgb(INK_FAINT)))
                    .child(Icon::new(IconAll::ArrowRight).size_4().text_color(rgb(INK_FAINT))),
            )
            .child(
                div()
                    .flex_1()
                    .h_flex()
                    .justify_center()
                    .child(
                        div()
                            .h_flex()
                            .gap_0p5()
                            .p(px(3.))
                            .rounded_lg()
                            .bg(rgb(SAND_ACTIVE))
                            .child(Self::top_tab("Chat", false))
                            .child(Self::top_tab("Cowork", false))
                            .child(Self::top_tab("Code", true)),
                    ),
            );

        // 自动跟随滚动：在渲染后把滚动钉到底部
        let active = self.active;
        if self.sessions[active].follow_scroll {
            self.sessions[active].scroll.scroll_to_bottom();
        }
        // 还没发过消息的会话显示首页（问候 + 概览）
        let home = !self.sessions[active].has_user_message();

        let sidebar = if self.sidebar_open {
            self.render_sidebar(cx)
        } else {
            v_flex()
                .w(px(48.))
                .flex_shrink_0()
                .h_full()
                .items_center()
                .pt_3()
                .bg(rgb(SAND))
                .border_r_1()
                .border_color(rgb(LINE))
                .child(self.sidebar_toggle("sidebar-toggle-collapsed", cx))
        };
        let composer_area = div().px_6().pb_4().pt_1().child(self.render_composer(cx));

        let main = if self.settings_open {
            v_flex()
                .flex_1()
                .min_w_0()
                .child(self.render_settings(cx))
        } else if home {
            // 首页：问候 + 用量概览
            v_flex()
                .flex_1()
                .min_w_0()
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
            let mut list = v_flex()
                .max_w(px(760.))
                .mx_auto()
                .w_full()
                .py_6()
                .px_6()
                .gap_4();
            for (idx, item) in self.sessions[active].transcript.iter().enumerate() {
                list = list.child(self.render_item(idx, item, cx));
            }

            let scroll = div()
                .id("transcript")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.sessions[active].scroll)
                .child(list);

            v_flex()
                .flex_1()
                .min_w_0()
                .relative()
                .child(self.render_top_bar(cx))
                .child(scroll)
                .child(composer_area)
                // 顶栏打开的模型下拉放在最后画，盖在对话上面
                .when(self.model_menu_open && self.model_menu_top, |d| {
                    d.child(self.render_model_menu(cx).top(px(50.)).right(px(100.)))
                })
        };

        div()
            .v_flex()
            .size_full()
            .font_family(SANS)
            .text_size(px(15.))
            .bg(rgb(PAPER))
            .text_color(rgb(INK))
            .child(title_bar)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(sidebar)
                    .child(main.h_full()),
            )
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
