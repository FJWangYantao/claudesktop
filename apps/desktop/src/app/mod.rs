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
use claudesktop_core::{
    AcpKernel, AcpKernelConfig, EventSink, Kernel, KernelEvent, Record, RoutedEvent, SessionStore,
};
use claudesktop_builtin::BuiltinKernel;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
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
    /// 内核启动命令（写进会话元信息）
    agent_cmd: String,
    /// 默认工作目录（新会话用）
    cwd: PathBuf,
    /// 当前项目名（取工作目录名），侧栏分组名与输入框标签用
    project_name: SharedString,
    overview_tab: usize, // 0 概览 / 1 模型
    overview_range: usize, // 0 全部 / 1 30 天 / 2 7 天
    sidebar_open: bool,
    mode_menu_open: bool,
    model_menu_open: bool,
    /// 当前内核；None = 纯演示回放（选了演示回放或启动失败）
    kernel: Option<Arc<dyn Kernel>>,
    /// 内核代号：每次启动 +1，事件带着代号回来，不是当前代号的丢弃
    kernel_gen: u64,
    ev_tx: UnboundedSender<(u64, RoutedEvent)>,
    /// 环境变量给的 ACP 基础配置（工作目录、调试开关）
    acp_base: AcpKernelConfig,
    /// 与 agent 的连接已建立（可以创建会话）
    kernel_ready: bool,
    kernel_error: bool,
    kernel_status: SharedString,
    /// 生效中的设置（启动时已合并环境变量）
    settings: Settings,
    settings_path: Option<PathBuf>,
    settings_open: bool,
    settings_form: SettingsForm,
    /// 正在后台获取模型列表
    fetching_models: bool,
}

impl Drop for ChatApp {
    fn drop(&mut self) {
        if let Some(kernel) = &self.kernel {
            kernel.shutdown();
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

        let (ev_tx, mut ev_rx) = futures::channel::mpsc::unbounded::<(u64, RoutedEvent)>();
        cx.spawn(async move |this, cx| {
            while let Some((generation, event)) = ev_rx.next().await {
                if this
                    .update(cx, |app, cx| {
                        if generation == app.kernel_gen {
                            app.on_kernel_event(event, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let settings_form = SettingsForm::new(window, cx);
        let mut app = Self {
            input,
            sessions: Vec::new(),
            active: 0,
            next_token: 1,
            store: None,
            agent_cmd: String::new(),
            cwd,
            project_name,
            overview_tab: 0,
            overview_range: 0,
            sidebar_open: true,
            mode_menu_open: false,
            model_menu_open: false,
            kernel: None,
            kernel_gen: 0,
            ev_tx,
            acp_base,
            kernel_ready: false,
            kernel_error: false,
            kernel_status: SharedString::default(),
            settings,
            settings_path,
            settings_open: false,
            settings_form,
            fetching_models: false,
        };
        app.spawn_kernel();

        // 第一个会话：连接就绪（Connected）后统一补发 session/new
        let ready = app.kernel.is_none();
        app.sessions.push(Session::new(app.next_token, ready, app.cwd.clone()));
        app.next_token += 1;
        if let Some(err) = settings_error {
            app.sessions[0].note(err);
        }
        // 从磁盘恢复历史会话（演示回放模式下新会话不落盘，但历史照常显示）
        match SessionStore::open_default() {
            Ok(store) => {
                for meta in store.list().into_iter().take(MAX_RESTORED) {
                    let records = store.load(&meta.id).unwrap_or_default();
                    app.sessions.push(Session::from_records(app.next_token, meta, records));
                    app.next_token += 1;
                }
                app.store = Some(store);
            }
            Err(err) => app.sessions[0].note(format!("会话历史不可用：{err}")),
        }
        app
    }

    /// 按当前设置启动内核（不处理会话，见 [`Self::restart_kernel`]）。
    fn spawn_kernel(&mut self) {
        self.kernel_gen += 1;
        let generation = self.kernel_gen;
        let tx = self.ev_tx.clone();
        let sink: EventSink = Arc::new(move |event| {
            let _ = tx.unbounded_send((generation, event));
        });
        self.kernel_ready = false;
        self.kernel_error = false;
        let (kernel, status, agent_cmd): (Option<Arc<dyn Kernel>>, SharedString, String) =
            match self.settings.kernel() {
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
                    Err(err) => (None, format!("自研内核启动失败：{err}").into(), String::new()),
                },
                KernelChoice::Acp => {
                    let config = self.settings.acp_config(self.acp_base.clone());
                    let agent_cmd = config.command.clone();
                    match AcpKernel::spawn(config, sink) {
                        Ok(k) => (Some(Arc::new(k) as Arc<dyn Kernel>), "内核启动中…".into(), agent_cmd),
                        Err(err) => (None, format!("内核启动失败：{err}").into(), agent_cmd),
                    }
                }
            };
        self.kernel_error = kernel.is_none() && self.settings.kernel() != KernelChoice::Demo;
        self.kernel = kernel;
        self.kernel_status = status;
        self.agent_cmd = agent_cmd;
    }

    /// 换内核（设置改了之后）：关掉旧的，所有会话脱离旧内核；
    /// 有对话的会话下次发消息时按「历史会话」续接，空会话等新连接就绪后重新创建。
    pub(crate) fn restart_kernel(&mut self, cx: &mut Context<Self>) {
        let old = self.kernel.take();
        for s in &mut self.sessions {
            s.detach_kernel(old.as_ref());
        }
        if let Some(old) = old {
            old.shutdown();
        }
        self.spawn_kernel();
        if self.kernel.is_none() {
            // 演示回放：所有会话直接可用
            for s in &mut self.sessions {
                s.ready = true;
                s.restored = false;
            }
        }
        cx.notify();
    }

    pub(crate) fn session(&self) -> &Session {
        &self.sessions[self.active]
    }

    /// 新建一个会话并切过去；连接已就绪就立刻向内核要，否则等 Connected 时补发。
    pub(crate) fn add_session(&mut self, cx: &mut Context<Self>) {
        let token = self.next_token;
        self.next_token += 1;
        self.sessions
            .push(Session::new(token, self.kernel.is_none(), self.cwd.clone()));
        self.active = self.sessions.len() - 1;
        self.request_pending_sessions();
        cx.notify();
    }

    /// 向内核要还没要过的会话（`session/new`）。连接未就绪时什么也不做。
    pub(crate) fn request_pending_sessions(&mut self) {
        if !self.kernel_ready {
            return;
        }
        let Some(kernel) = self.kernel.clone() else {
            return;
        };
        for s in &mut self.sessions {
            if !s.requested && !s.restored && s.acp_id.is_none() {
                s.requested = true;
                if let Err(err) = kernel.new_session(s.token, Some(s.cwd.clone())) {
                    s.requested = false;
                    s.note(format!("创建会话失败：{err}"));
                }
            }
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

    /// 向第 `idx` 个会话发一轮用户消息：有内核走内核，否则播放演示脚本。
    pub(crate) fn submit_to(&mut self, idx: usize, text: String, cx: &mut Context<Self>) {
        let kernel = self.kernel.clone();
        let mut start_demo_token = None;
        {
            let s = &mut self.sessions[idx];
            if s.busy {
                return;
            }
            if let (Some(kernel), true) = (&kernel, s.restored) {
                // 历史会话：先续接内核会话，就绪后再发这条消息。同一个 ACP agent 才尝试；
                // 自研内核用本地日志重建上下文，换了端点 / 模型也能续接
                s.restored = false;
                s.requested = true;
                s.resuming = true;
                s.autoprompt = Some(text);
                let agent_cmd = &self.agent_cmd;
                let builtin = kernel.kind() == "builtin";
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
                            Some(store) if builtin => {
                                store.load(&meta.id).unwrap_or_default()
                            }
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
                    s.note(format!("续接会话失败：{err}"));
                } else {
                    s.note("正在续接会话…");
                }
                cx.notify();
                return;
            }
            if kernel.is_some() && !s.ready {
                s.note("会话尚未就绪，请稍候再发送");
                cx.notify();
                return;
            }
            if !s.has_user_message() {
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
        let kernel = self.kernel.clone();
        let active = self.active;
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
        let kernel = self.kernel.clone();
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

    /// 内核事件分发：连接级事件自己处理，会话级事件按 sessionId / token 找到会话。
    pub(crate) fn on_kernel_event(&mut self, routed: RoutedEvent, cx: &mut Context<Self>) {
        let RoutedEvent { session, event } = routed;
        let kernel = self.kernel.clone();
        match event {
            KernelEvent::Connected { agent_name } => {
                self.kernel_ready = true;
                self.kernel_error = false;
                // 去掉 npm 作用域前缀（@scope/name → name），底栏空间有限
                let short = agent_name.rsplit('/').next().unwrap_or(&agent_name);
                self.kernel_status = format!("{short} · 已连接").into();
                self.request_pending_sessions();
            }
            KernelEvent::SessionFailed { token, message } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.token == token) {
                    s.requested = false;
                    s.resuming = false;
                    s.note(format!("创建会话失败：{message}"));
                }
            }
            KernelEvent::Fatal(message) => {
                self.kernel_ready = false;
                self.kernel_error = true;
                self.kernel_status = "内核错误".into();
                for s in &mut self.sessions {
                    s.on_fatal(&message, kernel.as_ref());
                }
            }
            event => {
                // 会话级事件：Started 按令牌认领，其余按 sessionId
                let idx = match &event {
                    KernelEvent::Started { token, .. } => {
                        self.sessions.iter().position(|s| s.token == *token)
                    }
                    _ => session.as_deref().and_then(|id| {
                        self.sessions
                            .iter()
                            .position(|s| s.acp_id.as_deref() == Some(id))
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
        if s.log.is_none() {
            let opened = match &s.meta {
                Some(meta) => store.append_log(&meta.id),
                None => store
                    .create(&s.title, s.cwd.clone(), &self.agent_cmd)
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
            meta.agent = self.agent_cmd.clone();
            meta.title = s.title.to_string();
            meta.kernel_session_id = s.acp_id.clone();
            meta.updated_ms = now_ms();
            let _ = store.save_meta(meta);
        }
    }

    pub(crate) fn start_demo(&mut self, idx: usize, token: u64, cx: &mut Context<Self>) {
        let script = build_demo_script();
        cx.spawn(async move |this, cx| {
            for step in script {
                cx.background_executor()
                    .timer(Duration::from_millis(step.0))
                    .await;
                let mut cancelled = false;
                let _ = this.update(cx, |app, cx| {
                    match app.sessions.get_mut(idx) {
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
                if let Some(s) = app.sessions.get_mut(idx) {
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
                .child(self.render_top_bar())
                .child(scroll)
                .child(composer_area)
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
