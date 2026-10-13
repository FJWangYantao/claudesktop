//! M1 UI 外壳（内核未接入 · 演示回放）
//!
//! 目标界面：Claude 桌面风（暖纸底 + 赤陶橙），含
//! - 自绘标题栏 + 侧栏（工作区分组 / 任务列表 / 内核卡）
//! - 会话视图：左缘头像轨道、思考块、工具卡、计划、流式 Markdown
//! - 空会话首页：问候语 + 用量概览（统计卡 + 热力图）
//! - 底部输入区：工作区 / 模式 chips、圆角输入条、权限模式栏

use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use claudesktop_core::{
    AcpKernel, AcpKernelConfig, EventSink, Kernel, KernelEvent, PermissionOptionView,
};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;

// ---------- 色板（Claude 品牌基准：暖纸底 + 赤陶橙） ----------
const PAPER: u32 = 0xFAF9F5;
const SAND: u32 = 0xF0EEE6;
const CORAL: u32 = 0xD97757;
const SAND_ACTIVE: u32 = 0xE8E3D6;
const INK: u32 = 0x3D3929;
const INK_SOFT: u32 = 0x87847C;
const INK_FAINT: u32 = 0xB5B0A4;
const LINE: u32 = 0xE5E1D5;
const CARD: u32 = 0xFFFFFF;
const GREEN: u32 = 0x7A9A65;
const RED: u32 = 0xC0504D;
const BLUE: u32 = 0x5B6FA8;
const BLUE_TINT: u32 = 0xE9EEF7;
// 字体：官方是自家的 Sans / Serif / Mono，无法分发，取 Windows 自带的近似款。
const SANS: &str = "Segoe UI Variable Text";
const MONO: &str = "Cascadia Mono";
const DIFF_ADD_BG: u32 = 0xE4F3DE;
const DIFF_ADD_FG: u32 = 0x3C6B2E;
const DIFF_DEL_BG: u32 = 0xFBE3DF;
const DIFF_DEL_FG: u32 = 0xA4443A;
// Claude 的标题使用衬线体；Windows 自带 Georgia 作近似，中文回退到系统衬线。
const SERIF: &str = "Georgia";

// ---------- 数据模型 ----------
#[derive(Clone, Copy, PartialEq)]
enum ToolPhase {
    Running,
    Done,
    Failed,
}

/// 工具调用：`● Name arg` + `└ result`（Claude Code 的紧凑样式）
struct ToolCard {
    /// 内核侧的 tool_call_id（演示脚本为 None）
    id: Option<String>,
    name: SharedString,
    arg: SharedString,
    result: SharedString,
    phase: ToolPhase,
}

/// diff 行：(' ' 上下文 / '+' 新增 / '-' 删除, 文本)
type DiffRows = &'static [(char, &'static str)];

struct PlanRow {
    text: SharedString,
    done: bool,
}

enum ChatItem {
    User(SharedString),
    Thinking {
        text: String,
        streaming: bool,
        open: bool,
    },
    Tool(ToolCard),
    /// agent 的权限请求；`resolved` 有值后按钮消失，显示所选结果
    Permission {
        id: u64,
        title: SharedString,
        options: Vec<PermissionOptionView>,
        resolved: Option<SharedString>,
    },
    Diff {
        path: &'static str,
        rows: DiffRows,
    },
    Plan(Vec<PlanRow>),
    Agent(String),
    Note(SharedString),
}

struct SessionEntry {
    title: SharedString,
    age: SharedString,
    dot: Option<u32>, // 部分会话带在线小圆点
}

struct Workspace {
    name: SharedString,
    sessions: Vec<SessionEntry>,
}

struct StatCard {
    label: &'static str,
    value: SharedString,
}

// ---------- 演示回放脚本 ----------
enum MockAction {
    BeginThinking,
    ThinkChunk(&'static str),
    EndThinking,
    BeginTool(&'static str, &'static str),
    ToolDone(&'static str),
    Diff(&'static str, DiffRows),
    BeginPlan,
    PlanRowAdd(&'static str),
    PlanRowDone(usize),
    BeginAgent,
    AgentChunk(&'static str),
    EndAgent,
    Note(&'static str),
}

struct MockStep(u64, MockAction);

/// 把长文本按固定字数切成流式 chunk 步骤。
fn stream_steps(
    text: &'static str,
    per: usize,
    delay: u64,
    make: fn(&'static str) -> MockAction,
) -> Vec<MockStep> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let n = rest
            .char_indices()
            .nth(per)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let (chunk, tail) = rest.split_at(n);
        out.push(MockStep(delay, make(chunk)));
        rest = tail;
    }
    out
}

const DEMO_DIFF: DiffRows = &[
    (' ', "pub trait Kernel: Send {"),
    ('-', "    fn send(&mut self, text: String);"),
    ('+', "    fn send(&mut self, text: String) -> Result<()>;"),
    ('+', "    fn cancel(&mut self);"),
    (' ', "}"),
];

fn build_demo_script() -> Vec<MockStep> {
    let mut s = Vec::new();

    // 1) 思考块
    s.push(MockStep(300, MockAction::BeginThinking));
    s.extend(stream_steps(
        "用户在验收 UI 外壳。先展示工具卡与计划，再输出流式正文，最后给回合小结。",
        5,
        36,
        MockAction::ThinkChunk,
    ));
    s.push(MockStep(200, MockAction::EndThinking));

    // 2) 读文件
    s.push(MockStep(
        450,
        MockAction::BeginTool("Read", "crates/core/src/lib.rs"),
    ));
    s.push(MockStep(
        700,
        MockAction::ToolDone("Read 128 lines · Kernel trait 与 KernelEvent 事件模型"),
    ));

    // 3) 改文件 + diff
    s.push(MockStep(
        450,
        MockAction::BeginTool("Edit", "crates/core/src/lib.rs"),
    ));
    s.push(MockStep(
        700,
        MockAction::ToolDone("Added 2 lines, removed 1 line"),
    ));
    s.push(MockStep(
        150,
        MockAction::Diff("crates/core/src/lib.rs", DEMO_DIFF),
    ));

    // 4) 跑构建
    s.push(MockStep(
        500,
        MockAction::BeginTool("Bash", "cargo build -p claudesktop"),
    ));
    s.push(MockStep(
        950,
        MockAction::ToolDone("Finished `dev` profile in 1.8s · 0 errors, 0 warnings"),
    ));

    // 5) 计划卡
    s.push(MockStep(400, MockAction::BeginPlan));
    for row in [
        "UI 外壳验收（布局 / 消息类型 / IME）",
        "接入 ACP 内核（session/update → 消息流）",
        "InProcess 内核 + Provider trait",
        "权限审批与 MCP 配置统一到 UI 侧",
    ] {
        s.push(MockStep(260, MockAction::PlanRowAdd(row)));
    }
    for i in 0..4 {
        s.push(MockStep(300, MockAction::PlanRowDone(i)));
    }

    // 6) 流式 Markdown 正文
    s.push(MockStep(350, MockAction::BeginAgent));
    s.extend(stream_steps(
        "## UI 外壳验收清单\n\n这轮回复来自**内置演示回放**——UI 已就位，内核按计划下一步接入 ACP。\n\n你在上面看到的消息类型：\n\n- **思考块**：可折叠，流式追加\n- **工具卡片**：读文件 / 跑构建，带「运行中 → 完成」状态\n- **计划**：左缘赤陶色标记的清单，逐项点亮\n- **Markdown 正文**：就是本块，`stream_fade` 流式渲染\n\n接入后，这里的事件流将直接映射自 ACP 的 `session/update` 通知，UI 不再需要任何演示脚本。\n\n> 布局 / 配色 / 交互有任何不顺眼的地方，现在改最便宜。",
        4,
        30,
        MockAction::AgentChunk,
    ));
    s.push(MockStep(250, MockAction::EndAgent));

    // 7) 回合小结
    s.push(MockStep(
        400,
        MockAction::Note("演示回合结束 · 内核未连接 · 下一步：ACP 接入"),
    ));
    s
}

/// 权限模式（名称沿用官方桌面版）：(名称, 说明, 图标, ACP mode id)
const PERMISSION_MODES: [(&str, &str, IconAll, &str); 4] = [
    ("Manual", "每次编辑和命令都先询问", IconAll::ShieldCheck, "default"),
    ("Accept edits", "自动接受文件编辑，命令仍询问", IconAll::SquarePen, "acceptEdits"),
    ("Plan", "只出计划，不改动文件", IconAll::ListTodo, "plan"),
    ("Auto", "由分类器判断，少打扰", IconAll::Zap, "auto"),
];

// ---------- 应用状态 ----------
pub struct ChatApp {
    input: Entity<InputState>,
    transcript: Vec<ChatItem>,
    workspaces: Vec<Workspace>,
    /// None = 首页（用量概览）；Some(i) = 打开第 i 个会话
    selected: Option<usize>,
    busy: bool,
    cancel_token: u64,
    scroll: ScrollHandle,
    follow_scroll: bool,
    overview_tab: usize, // 0 概览 / 1 模型
    overview_range: usize, // 0 全部 / 1 30 天 / 2 7 天
    sidebar_open: bool,
    mode_menu_open: bool,
    permission_mode: usize, // PERMISSION_MODES 下标
    /// 当前内核；None = 纯演示回放（CLAUDESKTOP_KERNEL=demo 或启动失败）
    kernel: Option<Arc<dyn Kernel>>,
    kernel_ready: bool,
    kernel_error: bool,
    kernel_status: SharedString,
    /// 自动演示：内核就绪后发送的提示词
    autoprompt: Option<String>,
}

impl Drop for ChatApp {
    fn drop(&mut self) {
        if let Some(kernel) = &self.kernel {
            kernel.shutdown();
        }
    }
}

/// 把 ACP 的工具标题拆成「名称 + 参数」：`Read /a/b` → (Read, /a/b)，
/// `` `ls -la` `` → (Bash, ls -la)，其余 → (Tool, 原标题)。
fn split_tool_title(title: &str) -> (SharedString, SharedString) {
    let t = title.trim();
    let is_name = |w: &str| {
        w.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && w.chars().all(|c| c.is_ascii_alphanumeric())
    };
    if t.starts_with('`') {
        return ("Bash".into(), t.trim_matches('`').to_string().into());
    }
    if let Some((head, rest)) = t.split_once(' ') {
        if is_name(head) {
            return (head.to_string().into(), rest.trim().to_string().into());
        }
    }
    if is_name(t) {
        return (t.to_string().into(), SharedString::default());
    }
    ("Tool".into(), t.to_string().into())
}

fn tool_phase(status: &str) -> ToolPhase {
    match status {
        "completed" => ToolPhase::Done,
        "failed" => ToolPhase::Failed,
        _ => ToolPhase::Running,
    }
}

/// 工具原始输出 → 一行摘要（去引号/换行，截断）。
fn summarize_output(raw: &str) -> String {
    let t = raw.trim().trim_matches('"').replace("\\n", " ").replace("\\\"", "\"");
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 140 {
        format!("{}…", t.chars().take(140).collect::<String>())
    } else {
        t
    }
}

impl ChatApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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

        let workspaces = vec![
            Workspace {
                name: "claudesktop".into(),
                sessions: vec![
                    SessionEntry { title: "新会话".into(), age: "".into(), dot: None },
                    SessionEntry { title: "gpui 高性能 Claude 风格桌…".into(), age: "28 分钟".into(), dot: None },
                ],
            },
            Workspace {
                name: "ubuntu".into(),
                sessions: vec![
                    SessionEntry { title: "服务器能否调用 deepseek 搜索…".into(), age: "1 天".into(), dot: Some(GREEN) },
                    SessionEntry { title: "将 Clash for Windows 订阅导入…".into(), age: "1 天".into(), dot: Some(GREEN) },
                    SessionEntry { title: "探查当前目录".into(), age: "2 天".into(), dot: Some(GREEN) },
                ],
            },
            Workspace {
                name: "news_collector_integration".into(),
                sessions: vec![
                    SessionEntry { title: "帮我搜索 dsh 插件，能够不在工…".into(), age: "3 天".into(), dot: None },
                ],
            },
            Workspace {
                name: "work_station".into(),
                sessions: vec![
                    SessionEntry { title: "看一下我电脑上 antigravity 安装…".into(), age: "9 分钟".into(), dot: None },
                    SessionEntry { title: "Investigating zcode codingplan…".into(), age: "1 天".into(), dot: None },
                ],
            },
        ];

        // 内核：默认 ACP（ACP_AGENT 未设置时用仓库内 mock agent）；
        // CLAUDESKTOP_KERNEL=demo 退回纯演示回放。
        let demo_only = std::env::var("CLAUDESKTOP_KERNEL").ok().as_deref() == Some("demo");
        let (ev_tx, mut ev_rx) = futures::channel::mpsc::unbounded::<KernelEvent>();
        let (kernel, kernel_status): (Option<Arc<dyn Kernel>>, SharedString) = if demo_only {
            (None, "演示回放（未连接内核）".into())
        } else {
            let sink: EventSink = Arc::new(move |event| {
                let _ = ev_tx.unbounded_send(event);
            });
            match AcpKernel::spawn(AcpKernelConfig::from_env(), sink) {
                Ok(k) => (Some(Arc::new(k) as Arc<dyn Kernel>), "内核启动中…".into()),
                Err(err) => (None, format!("内核启动失败：{err}").into()),
            }
        };
        cx.spawn(async move |this, cx| {
            while let Some(event) = ev_rx.next().await {
                if this
                    .update(cx, |app, cx| app.on_kernel_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        Self {
            input,
            transcript: Vec::new(),
            workspaces,
            selected: Some(0),
            busy: false,
            cancel_token: 0,
            scroll: ScrollHandle::new(),
            follow_scroll: true,
            overview_tab: 0,
            overview_range: 0,
            sidebar_open: true,
            mode_menu_open: false,
            permission_mode: 0,
            kernel,
            kernel_ready: false,
            kernel_error: false,
            kernel_status,
            autoprompt: None,
        }
    }

    /// 调试钩子：CLAUDESKTOP_AUTODEMO=1 时自动发一条提示词
    /// （有内核则等内核就绪后经内核发送，否则播放演示脚本）。
    fn autodemo_hook(&mut self, cx: &mut Context<Self>) {
        if std::env::var("CLAUDESKTOP_AUTODEMO").ok().as_deref() != Some("1") {
            return;
        }
        let text = "帮我配置一个有图形界面的动态壁纸软件".to_string();
        self.selected = Some(0);
        if self.kernel.is_some() && !self.kernel_ready {
            self.autoprompt = Some(text);
        } else {
            self.submit(text, cx);
        }
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let text = self.input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        // 从首页发消息 → 自动进入会话视图
        if self.selected.is_none() {
            self.selected = Some(0);
        }
        self.input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.submit(text, cx);
    }

    /// 发送一轮用户消息：有内核走内核，否则播放演示脚本。
    fn submit(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(kernel) = self.kernel.clone() {
            if !self.kernel_ready {
                self.transcript
                    .push(ChatItem::Note("内核尚未就绪，请稍候再发送".into()));
                cx.notify();
                return;
            }
            self.transcript
                .push(ChatItem::User(SharedString::from(text.clone())));
            self.busy = true;
            self.follow_scroll = true;
            if let Err(err) = kernel.send_prompt(text) {
                self.busy = false;
                self.transcript
                    .push(ChatItem::Note(format!("发送失败：{err}").into()));
            }
        } else {
            self.transcript.push(ChatItem::User(SharedString::from(text)));
            self.busy = true;
            self.follow_scroll = true;
            self.cancel_token += 1;
            self.start_demo(cx);
        }
        cx.notify();
    }

    /// 应答某条权限请求：`Some((选项 ID, 展示名))` 选择，`None` 取消。
    fn resolve_permission(
        &mut self,
        idx: usize,
        choice: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let Some(kernel) = self.kernel.clone() else {
            return;
        };
        if let Some(ChatItem::Permission { id, resolved, .. }) = self.transcript.get_mut(idx) {
            if resolved.is_some() {
                return;
            }
            let label: SharedString = match &choice {
                Some((_, name)) => name.clone().into(),
                None => "已取消".into(),
            };
            *resolved = Some(match kernel.resolve_permission(*id, choice.map(|(i, _)| i)) {
                Ok(()) => label,
                Err(err) => format!("应答失败：{err}").into(),
            });
        }
        cx.notify();
    }

    /// 本轮结束 / 停止 / 内核出错时，仍在等待的请求一律按「取消」收尾。
    fn cancel_pending_permissions(&mut self) {
        let Some(kernel) = self.kernel.clone() else {
            return;
        };
        for item in &mut self.transcript {
            if let ChatItem::Permission { id, resolved, .. } = item {
                if resolved.is_none() {
                    let _ = kernel.resolve_permission(*id, None);
                    *resolved = Some("已取消".into());
                }
            }
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.cancel_token += 1;
        self.busy = false;
        self.cancel_pending_permissions();
        let note = match &self.kernel {
            Some(kernel) => {
                let _ = kernel.cancel();
                "已请求停止"
            }
            None => "已停止（演示回放）",
        };
        self.transcript.push(ChatItem::Note(note.into()));
        cx.notify();
    }

    fn finish_thinking(&mut self) {
        if let Some(ChatItem::Thinking { streaming, open, .. }) = self.transcript.last_mut() {
            *streaming = false;
            *open = false;
        }
    }

    /// 内核事件 → 界面消息（ACP `session/update` 的 UI 投影）。
    fn on_kernel_event(&mut self, event: KernelEvent, cx: &mut Context<Self>) {
        match event {
            KernelEvent::Status(_) => {}
            KernelEvent::Started { agent_name, .. } => {
                self.kernel_ready = true;
                self.kernel_error = false;
                // 去掉 npm 作用域前缀（@scope/name → name），底栏空间有限
                let short = agent_name.rsplit('/').next().unwrap_or(&agent_name);
                self.kernel_status = format!("{short} · 已连接").into();
                if let Some(text) = self.autoprompt.take() {
                    self.submit(text, cx);
                }
            }
            KernelEvent::AgentMessageChunk { text } => {
                self.finish_thinking();
                match self.transcript.last_mut() {
                    Some(ChatItem::Agent(buf)) => buf.push_str(&text),
                    _ => self.transcript.push(ChatItem::Agent(text)),
                }
            }
            KernelEvent::AgentThoughtChunk { text } => match self.transcript.last_mut() {
                Some(ChatItem::Thinking {
                    text: buf,
                    streaming: true,
                    ..
                }) => buf.push_str(&text),
                _ => self.transcript.push(ChatItem::Thinking {
                    text,
                    streaming: true,
                    open: true,
                }),
            },
            KernelEvent::UserMessageChunk { .. } => {}
            KernelEvent::ToolCallStarted { id, title, status } => {
                self.finish_thinking();
                let (name, arg) = split_tool_title(&title);
                self.transcript.push(ChatItem::Tool(ToolCard {
                    id: Some(id),
                    name,
                    arg,
                    result: SharedString::default(),
                    phase: tool_phase(&status),
                }));
            }
            KernelEvent::ToolCallUpdated {
                id,
                title,
                status,
                raw_output,
            } => {
                let card = self.transcript.iter_mut().rev().find_map(|item| match item {
                    ChatItem::Tool(card) if card.id.as_deref() == Some(id.as_str()) => Some(card),
                    _ => None,
                });
                if let Some(card) = card {
                    if let Some(title) = title {
                        let (name, arg) = split_tool_title(&title);
                        card.name = name;
                        card.arg = arg;
                    }
                    if let Some(status) = status {
                        card.phase = tool_phase(&status);
                    }
                    if let Some(raw) = raw_output {
                        card.result = summarize_output(&raw).into();
                    }
                }
            }
            KernelEvent::PlanReplaced { entries } => {
                self.finish_thinking();
                let rows: Vec<PlanRow> = entries
                    .into_iter()
                    .map(|e| PlanRow {
                        done: e.status == "completed",
                        text: e.content.into(),
                    })
                    .collect();
                let turn_start = self
                    .transcript
                    .iter()
                    .rposition(|i| matches!(i, ChatItem::User(_)))
                    .map_or(0, |i| i + 1);
                let existing = self.transcript[turn_start..]
                    .iter()
                    .rposition(|i| matches!(i, ChatItem::Plan(_)))
                    .map(|i| i + turn_start);
                match existing {
                    Some(i) => self.transcript[i] = ChatItem::Plan(rows),
                    None => self.transcript.push(ChatItem::Plan(rows)),
                }
            }
            KernelEvent::PermissionAuto { title, chosen } => {
                self.transcript.push(ChatItem::Note(
                    format!("已自动批准：{title} · {chosen}").into(),
                ));
            }
            KernelEvent::PermissionRequest { id, title, options } => {
                self.finish_thinking();
                self.transcript.push(ChatItem::Permission {
                    id,
                    title: title.into(),
                    options,
                    resolved: None,
                });
            }
            KernelEvent::FsRead { .. } | KernelEvent::FsWrite { .. } => {}
            KernelEvent::PromptFailed { message } => {
                self.finish_thinking();
                self.busy = false;
                self.cancel_pending_permissions();
                self.transcript
                    .push(ChatItem::Note(format!("本轮失败：{message}").into()));
            }
            KernelEvent::TurnEnded { stop_reason } => {
                self.finish_thinking();
                self.busy = false;
                self.cancel_pending_permissions();
                if stop_reason != "end_turn" {
                    self.transcript
                        .push(ChatItem::Note(format!("本轮结束：{stop_reason}").into()));
                }
            }
            KernelEvent::Fatal(message) => {
                self.busy = false;
                self.cancel_pending_permissions();
                self.kernel_ready = false;
                self.kernel_error = true;
                self.kernel_status = "内核错误".into();
                self.transcript
                    .push(ChatItem::Note(format!("内核错误：{message}").into()));
            }
        }
        cx.notify();
    }

    fn start_demo(&mut self, cx: &mut Context<Self>) {
        let script = build_demo_script();
        let token = self.cancel_token;
        cx.spawn(async move |this, cx| {
            for step in script {
                cx.background_executor()
                    .timer(Duration::from_millis(step.0))
                    .await;
                let mut cancelled = false;
                let _ = this.update(cx, |app, cx| {
                    if app.cancel_token != token {
                        cancelled = true;
                        return;
                    }
                    app.apply_mock(&step.1);
                    cx.notify();
                });
                if cancelled {
                    return;
                }
            }
            let _ = this.update(cx, |app, cx| {
                app.busy = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_mock(&mut self, action: &MockAction) {
        match action {
            MockAction::BeginThinking => {
                self.transcript.push(ChatItem::Thinking {
                    text: String::new(),
                    streaming: true,
                    open: true,
                });
            }
            MockAction::ThinkChunk(chunk) => {
                if let Some(ChatItem::Thinking { text, .. }) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndThinking => {
                if let Some(ChatItem::Thinking { streaming, open, .. }) =
                    self.transcript.last_mut()
                {
                    *streaming = false;
                    *open = false;
                }
            }
            MockAction::BeginTool(name, arg) => {
                self.transcript.push(ChatItem::Tool(ToolCard {
                    id: None,
                    name: (*name).into(),
                    arg: (*arg).into(),
                    result: SharedString::default(),
                    phase: ToolPhase::Running,
                }));
            }
            MockAction::ToolDone(result) => {
                if let Some(ChatItem::Tool(card)) = self.transcript.last_mut() {
                    card.result = (*result).into();
                    card.phase = ToolPhase::Done;
                }
            }
            MockAction::Diff(path, rows) => {
                self.transcript.push(ChatItem::Diff { path, rows });
            }
            MockAction::BeginPlan => {
                self.transcript.push(ChatItem::Plan(Vec::new()));
            }
            MockAction::PlanRowAdd(row) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    rows.push(PlanRow {
                        text: (*row).into(),
                        done: false,
                    });
                }
            }
            MockAction::PlanRowDone(i) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    if let Some(row) = rows.get_mut(*i) {
                        row.done = true;
                    }
                }
            }
            MockAction::BeginAgent => {
                self.transcript.push(ChatItem::Agent(String::new()));
            }
            MockAction::AgentChunk(chunk) => {
                if let Some(ChatItem::Agent(text)) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndAgent => {}
            MockAction::Note(text) => {
                self.transcript.push(ChatItem::Note((*text).into()));
            }
        }
    }

    // ---------- 小组件 ----------
    fn chip(icon: IconAll, label: impl Into<SharedString>) -> Div {
        div()
            .h_flex()
            .gap_1p5()
            .px_2p5()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .child(
                Icon::new(icon)
                    .size_3p5()
                    .text_color(rgb(INK_SOFT)),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(rgb(INK))
                    .child(label.into()),
            )
    }

    /// 标题栏中间的 Chat / Cowork / Code 分段切换。
    fn top_tab(text: &'static str, active: bool) -> Div {
        div()
            .px_3p5()
            .py_1()
            .rounded_md()
            .text_size(px(14.))
            .bg(if active { rgb(CARD) } else { rgba(0x00000000) })
            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
            .child(text)
    }

    fn seg(text: &'static str, active: bool) -> Div {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .text_size(px(13.))
            .bg(if active { rgb(SAND_ACTIVE) } else { rgba(0x00000000) })
            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
            .child(text)
    }

    // ---------- 消息渲染 ----------
    fn render_thinking(
        idx: usize,
        text: &str,
        streaming: bool,
        open: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let caret = if streaming { " ▍" } else { "" };
        let header = div()
            .id(("think-toggle", idx))
            .h_flex()
            .gap_1p5()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(ChatItem::Thinking { open, .. }) = this.transcript.get_mut(idx) {
                    *open = !*open;
                    cx.notify();
                }
            }))
            .child(
                Icon::new(IconAll::Brain)
                    .size_3p5()
                    .text_color(rgb(INK_SOFT)),
            )
            .child(
                div().text_size(px(12.5)).text_color(rgb(INK_SOFT)).child(if streaming {
                    SharedString::from("思考中…")
                } else {
                    SharedString::from("已思考")
                }),
            )
            .child(
                Icon::new(if open {
                    IconAll::ChevronDown
                } else {
                    IconAll::ChevronRight
                })
                .size_3()
                .text_color(rgb(INK_SOFT)),
            );

        let mut col = div().v_flex().gap_1().child(header);
        if open {
            col = col.child(
                div()
                    .text_size(px(13.))
                    .text_color(rgb(INK_SOFT))
                    .line_height(relative(1.6))
                    .child(format!("{text}{caret}")),
            );
        }
        col
    }

    fn render_tool(card: &ToolCard) -> Div {
        let dot = match card.phase {
            ToolPhase::Running => CORAL,
            ToolPhase::Done => GREEN,
            ToolPhase::Failed => RED,
        };
        let mut col = div().v_flex().gap(px(2.)).child(
            div()
                .h_flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .size(px(7.))
                        .mt(px(7.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(rgb(dot)),
                )
                .child(
                    div()
                        .text_size(px(14.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(INK))
                        .child(card.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.5))
                        .font_family(MONO)
                        .text_color(rgb(INK_SOFT))
                        .line_height(relative(1.7))
                        .child(card.arg.clone()),
                ),
        );
        if !card.result.is_empty() {
            col = col.child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_2()
                    .pl_3()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_family(MONO)
                            .text_color(rgb(INK_FAINT))
                            .child("└"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.5))
                            .font_family(MONO)
                            .text_color(rgb(INK_SOFT))
                            .child(card.result.clone()),
                    ),
            );
        }
        col
    }

    fn render_permission(
        idx: usize,
        title: &SharedString,
        options: &[PermissionOptionView],
        resolved: &Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> Div {
        let header = div()
            .h_flex()
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconAll::ShieldAlert)
                    .size_4()
                    .text_color(rgb(CORAL)),
            )
            .child(
                div()
                    .text_size(px(14.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("请求权限"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.5))
                    .font_family(MONO)
                    .text_color(rgb(INK_SOFT))
                    .child(title.clone()),
            );

        let footer = match resolved {
            Some(label) => div()
                .text_size(px(13.5))
                .text_color(rgb(INK_SOFT))
                .child(format!("已选择：{label}")),
            None => {
                let mut row = div().h_flex().gap_2();
                for (n, opt) in options.iter().enumerate() {
                    let allow = opt.kind.starts_with("allow");
                    let primary = opt.kind == "allow_once";
                    let (id, name) = (opt.id.clone(), opt.name.clone());
                    row = row.child(
                        div()
                            .id(("perm", idx * 16 + n))
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .cursor_pointer()
                            .text_size(px(13.5))
                            .when(primary, |d| d.bg(rgb(CORAL)).text_color(rgb(0xFFFFFF)))
                            .when(!primary, |d| {
                                d.border_1()
                                    .border_color(rgb(LINE))
                                    .bg(rgb(CARD))
                                    .hover(|d| d.bg(rgb(SAND)))
                                    .text_color(rgb(if allow { INK } else { RED }))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.resolve_permission(
                                    idx,
                                    Some((id.clone(), name.clone())),
                                    cx,
                                );
                            }))
                            .child(opt.name.clone()),
                    );
                }
                row
            }
        };

        div()
            .v_flex()
            .gap_2p5()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(CORAL))
            .bg(rgb(CARD))
            .child(header)
            .child(footer)
    }

    fn render_diff(path: &'static str, rows: DiffRows) -> Div {
        let mut old_no = 1usize;
        let mut new_no = 1usize;
        let mut body = div().v_flex();
        for (kind, text) in rows {
            let (bg, fg, no) = match kind {
                '+' => {
                    let n = new_no;
                    new_no += 1;
                    (Some(DIFF_ADD_BG), DIFF_ADD_FG, n)
                }
                '-' => {
                    let n = old_no;
                    old_no += 1;
                    (Some(DIFF_DEL_BG), DIFF_DEL_FG, n)
                }
                _ => {
                    let n = new_no;
                    old_no += 1;
                    new_no += 1;
                    (None, INK_SOFT, n)
                }
            };
            body = body.child(
                div()
                    .h_flex()
                    .items_start()
                    .when_some(bg, |d, c| d.bg(rgb(c)))
                    .font_family(MONO)
                    .text_size(px(13.5))
                    .line_height(relative(1.6))
                    .child(
                        div()
                            .w(px(36.))
                            .flex_shrink_0()
                            .pr_2()
                            .flex()
                            .justify_end()
                            .text_color(rgb(INK_FAINT))
                            .child(format!("{no}")),
                    )
                    .child(
                        div()
                            .w(px(14.))
                            .flex_shrink_0()
                            .text_color(rgb(fg))
                            .child(kind.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(rgb(if bg.is_some() { fg } else { INK }))
                            .child(*text),
                    ),
            );
        }
        div()
            .v_flex()
            .rounded_lg()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .overflow_hidden()
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .px_3()
                    .py_1p5()
                    .bg(rgb(SAND))
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        Icon::new(IconAll::FileText)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_family(MONO)
                            .text_color(rgb(INK_SOFT))
                            .child(path),
                    ),
            )
            .child(body)
    }

    fn render_plan(rows: &[PlanRow]) -> Div {
        div()
            .v_flex()
            .gap_1p5()
            .p_3()
            .rounded_lg()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .child(
                        Icon::new(IconAll::ListTodo)
                            .size_4()
                            .text_color(rgb(CORAL)),
                    )
                    .child(div().text_size(px(14.)).text_color(rgb(INK)).child("计划")),
            )
            .children(rows.iter().enumerate().map(|(i, row)| {
                let marker = if row.done {
                    div()
                        .size(px(15.))
                        .rounded_full()
                        .bg(rgb(GREEN))
                        .flex()
                        .justify_center()
                        .items_center()
                        .child(
                            Icon::new(IconAll::Check)
                                .size_3()
                                .text_color(rgb(0xFFFFFF)),
                        )
                } else {
                    div()
                        .size(px(15.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(LINE))
                };
                div()
                    .id(("plan-row", i))
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(marker)
                    .child(
                        div()
                            .text_size(px(13.5))
                            .text_color(if row.done {
                                rgb(INK_SOFT)
                            } else {
                                rgb(INK)
                            })
                            .child(row.text.clone()),
                    )
            }))
    }

    /// 助手回合行：官方界面没有头像轨道，内容直接顶格。
    fn railed(content: impl IntoElement) -> Div {
        div().min_w_0().child(content)
    }

    fn render_item(&self, idx: usize, item: &ChatItem, cx: &mut Context<Self>) -> Div {
        match item {
            ChatItem::User(text) => div()
                .w_full()
                .bg(rgb(SAND))
                .text_color(rgb(INK))
                .rounded(px(12.))
                .px_4()
                .py_3()
                .text_size(px(15.))
                .line_height(relative(1.6))
                .child(text.clone()),
            ChatItem::Thinking {
                text,
                streaming,
                open,
            } => Self::railed(Self::render_thinking(idx, text, *streaming, *open, cx)),
            ChatItem::Tool(card) => Self::railed(Self::render_tool(card)),
            ChatItem::Diff { path, rows } => Self::railed(Self::render_diff(path, rows)),
            ChatItem::Permission {
                title,
                options,
                resolved,
                ..
            } => Self::railed(Self::render_permission(idx, title, options, resolved, cx)),
            ChatItem::Plan(rows) => Self::railed(Self::render_plan(rows)),
            ChatItem::Agent(text) => Self::railed(
                div().text_size(px(15.5)).line_height(relative(1.7)).child(
                    TextView::markdown(("agent", idx), SharedString::from(text.clone()))
                        .style(
                            TextViewStyle::default()
                                .paragraph_gap(rems(0.9))
                                .heading_font_size(|level, _| match level {
                                    1 => px(22.),
                                    2 => px(19.),
                                    3 => px(17.),
                                    _ => px(15.5),
                                }),
                        )
                        .stream_fade(true),
                ),
            ),
            ChatItem::Note(text) => {
                div().h_flex().justify_center().pt_1().child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(INK_FAINT))
                        .child(text.clone()),
                )
            }
        }
    }

    // ---------- 首页（用量概览） ----------
    fn heat_color(v: u8) -> u32 {
        match v {
            0 => 0xE7E2D6,   // 空白格：沙色
            1 => 0xD8DFF0,   // 浅蓝
            2 => 0xA9B9DC,   // 中蓝
            3 => 0x7C92C4,   // 深蓝
            _ => 0x4C62A6,   // 最深
        }
    }

    fn render_overview(&self, cx: &mut Context<Self>) -> Div {
        // 热力图假数据：6 行 × 26 列，0..=4 强度
        let mut heat: Vec<u8> = Vec::with_capacity(6 * 26);
        let mut seed: u32 = 0x9E3779B9;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 100) as u8
        };
        for i in 0..(6 * 26) {
            let r = next();
            // 稀疏分布：大多数为 0
            let v = if r < 78 {
                0
            } else if r < 88 {
                1
            } else if r < 94 {
                2
            } else if r < 98 {
                3
            } else {
                4
            };
            // 末尾几天制造最近活跃
            let v = if i >= 6 * 26 - 3 && v == 0 { (i % 3 + 2) as u8 } else { v };
            heat.push(v);
        }

        let stats = [
            StatCard { label: "会话数", value: "87".into() },
            StatCard { label: "消息数", value: "4,194".into() },
            StatCard { label: "Token 总量", value: "299.4M".into() },
            StatCard { label: "活跃天数", value: "9".into() },
            StatCard { label: "高峰时段", value: "下午 12 点".into() },
            StatCard { label: "最常用模型", value: "GLM-5.3".into() },
        ];

        let mut grid = div().v_flex().gap(px(3.));
        for row in 0..6 {
            let mut line = div().h_flex().gap(px(3.));
            for col in 0..26 {
                let v = heat[row * 26 + col];
                line = line.child(
                    div()
                        .size(px(14.))
                        .rounded(px(3.))
                        .bg(rgb(Self::heat_color(v))),
                );
            }
            grid = grid.child(line);
        }

        div()
            .v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_6()
            .child(
                // 问候语
                div()
                    .h_flex()
                    .gap_2p5()
                    .items_center()
                    .child(
                        Icon::new(IconAll::Sparkles)
                            .size_5()
                            .text_color(rgb(CORAL)),
                    )
                    .child(
                        div()
                            .font_family(SERIF)
                            .text_size(px(34.))
                            .text_color(rgb(INK))
                            .child("What's up next, WYT?"),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .gap_4()
                    .p_5()
                    .rounded_xl()
                    .bg(rgb(SAND))
                    .child(
                        // 顶行：左侧 tabs，右侧范围
                        div()
                            .h_flex()
                            .items_center()
                            .child(
                                div()
                                    .h_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .id("tab-overview")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_tab = 0;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("概览", self.overview_tab == 0)),
                                    )
                                    .child(
                                        div()
                                            .id("tab-model")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_tab = 1;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("模型", self.overview_tab == 1)),
                                    ),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .h_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .id("range-all")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_range = 0;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("全部", self.overview_range == 0)),
                                    )
                                    .child(
                                        div()
                                            .id("range-30")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_range = 1;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("30 天", self.overview_range == 1)),
                                    )
                                    .child(
                                        div()
                                            .id("range-7")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_range = 2;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("7 天", self.overview_range == 2)),
                                    ),
                            ),
                    )
                    .child(
                        // 统计卡：2 行 × 3 列
                        div()
                            .v_flex()
                            .gap_3()
                            .child(
                                div()
                                    .h_flex()
                                    .gap_3()
                                    .children(stats[..3].iter().map(|c| {
                                        div()
                                            .v_flex()
                                            .gap_1()
                                            .w(px(220.))
                                            .p_3()
                                            .rounded_lg()
                                            .bg(rgb(CARD))
                                            .child(
                                                div()
                                                    .text_size(px(12.5))
                                                    .text_color(rgb(INK_SOFT))
                                                    .child(c.label),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(17.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(INK))
                                                    .child(c.value.clone()),
                                            )
                                    })),
                            )
                            .child(
                                div()
                                    .h_flex()
                                    .gap_3()
                                    .children(stats[3..].iter().map(|c| {
                                        div()
                                            .v_flex()
                                            .gap_1()
                                            .w(px(220.))
                                            .p_3()
                                            .rounded_lg()
                                            .bg(rgb(CARD))
                                            .child(
                                                div()
                                                    .text_size(px(12.5))
                                                    .text_color(rgb(INK_SOFT))
                                                    .child(c.label),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(17.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(INK))
                                                    .child(c.value.clone()),
                                            )
                                    })),
                            ),
                    )
                    .child(grid)
                    .child(
                        div()
                            .pt_1()
                            .text_size(px(13.))
                            .text_color(rgb(INK_SOFT))
                            .child("你的 Token 用量相当于 3,333 本《善恶的彼岸》。"),
                    ),
            )
    }

    // ---------- 侧栏 ----------
    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let mut body = v_flex().gap_0().flex_1().min_h_0().overflow_hidden();
        let mut flat_idx = 0usize;

        // 工作区头部行
        body = body.child(
            div()
                .h_flex()
                .items_center()
                .px_3()
                .pt_3()
                .pb_1()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(INK_SOFT))
                        .child("工作区"),
                )
                .child(div().flex_1())
                .child(
                    Icon::new(IconAll::ArrowUpDown)
                        .size_3p5()
                        .text_color(rgb(INK_SOFT)),
                )
                .child(
                    div().pl_2().child(
                        Icon::new(IconAll::ExternalLink)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    ),
                ),
        );

        for ws in &self.workspaces {
            // 工作区名
            body = body.child(
                div()
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .text_size(px(13.5))
                    .text_color(rgb(INK))
                    .child(ws.name.clone()),
            );
            for s in &ws.sessions {
                let active = self.selected == Some(flat_idx);
                let this_idx = flat_idx;
                let mut row = div()
                    .id(("session", flat_idx))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py(px(5.))
                    .rounded_md()
                    .bg(if active { rgb(SAND_ACTIVE) } else { rgba(0x00000000) })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(this_idx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(5.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(if let Some(c) = s.dot { rgb(c) } else { rgb(LINE) }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(14.))
                            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
                            .child(s.title.clone()),
                    );
                if !s.age.is_empty() {
                    row = row.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .text_color(rgb(INK_FAINT))
                            .child(s.age.clone()),
                    );
                }
                body = body.child(row);
                flat_idx += 1;
            }
        }

        // 任务分组
        body = body.child(
            div()
                .px_3()
                .pt_4()
                .pb_1()
                .text_size(px(12.))
                .text_color(rgb(INK_SOFT))
                .child("任务"),
        );
        for (i, (title, age)) in [
            ("看一下我电脑上 antigravity to…", "9 分钟"),
            ("Investigating zcode codingpla…", "1 天"),
            ("检查一下我的 dsh 的插件，有一…", "2 天"),
        ]
        .iter()
        .enumerate()
        {
            body = body.child(
                div()
                    .id(("task", i))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py(px(5.))
                    .rounded_md()
                    .cursor_pointer()
                    .child(
                        div().size(px(5.)).flex_shrink_0().rounded_full().bg(rgb(LINE)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(14.))
                            .text_color(rgb(INK_SOFT))
                            .child(SharedString::from(*title)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .text_color(rgb(INK_FAINT))
                            .child(SharedString::from(*age)),
                    ),
            );
        }

        v_flex()
            .w(px(248.))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(SAND))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                // 品牌行
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .child(
                        Icon::new(IconAll::Sparkles)
                            .size_4()
                            .text_color(rgb(CORAL)),
                    )
                    .child(
                        div()
                            .font_family(SERIF)
                            .text_size(px(18.))
                            .text_color(rgb(INK))
                            .child("Claude"),
                    )
                    .child(div().flex_1())
                    .child(self.sidebar_toggle("sidebar-toggle", cx)),
            )
            .child(
                div()
                    .id("new-chat")
                    .h_flex()
                    .gap_2()
                    .mx_2()
                    .mt_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected = None;
                        this.transcript.clear();
                        cx.notify();
                    }))
                    .child(
                        Icon::new(IconAll::SquarePen)
                            .size_3p5()
                            .text_color(rgb(INK)),
                    )
                    .child(div().text_size(px(14.)).text_color(rgb(INK)).child("新会话")),
            )
            .child(
                div()
                    .id("plugins")
                    .h_flex()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .child(
                        Icon::new(IconAll::Blocks)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(div().text_size(px(14.)).text_color(rgb(INK_SOFT)).child("插件")),
            )
            .child(div().flex_1().min_h_0().child(body))
            .child(
                // 底部：更多 + 内核状态
                div()
                    .v_flex()
                    .gap_2()
                    .p_3()
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Icon::new(IconAll::Ellipsis)
                                    .size_3p5()
                                    .text_color(rgb(INK_SOFT)),
                            )
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child("更多"),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(div().size(px(6.)).rounded_full().bg(rgb(
                                if self.kernel_error {
                                    RED
                                } else if self.kernel_ready {
                                    GREEN
                                } else {
                                    CORAL
                                },
                            )))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(12.))
                                    .text_color(rgb(INK_SOFT))
                                    .child(self.kernel_status.clone()),
                            ),
                    ),
            )
    }

    // ---------- 会话区顶栏 ----------
    fn render_top_bar(&self) -> Div {
        div()
            .h_flex()
            .items_center()
            .gap_2p5()
            .px_5()
            .py_2p5()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .size(px(18.))
                            .rounded_md()
                            .bg(rgb(0xC2694E))
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(0xFFFFFF))
                                    .child("甲"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(14.5))
                            .text_color(rgb(INK))
                            .child("帮我配置一个有图形界面的动态壁纸软件"),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .px_2p5()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(BLUE_TINT))
                    .child(
                        Icon::new(IconAll::Folder)
                            .size_3p5()
                            .text_color(rgb(BLUE)),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgb(BLUE))
                            .child("claudesktop"),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .px_2p5()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .child(
                        Icon::new(IconAll::Bot)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(INK))
                            .child("GLM-5.3"),
                    )
                    .child(
                        Icon::new(IconAll::ChevronDown)
                            .size_3()
                            .text_color(rgb(INK_SOFT)),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .px_2p5()
                    .py_1()
                    .child(
                        Icon::new(IconAll::Share)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(INK_SOFT))
                            .child("分享"),
                    ),
            )
    }

    // ---------- 输入区 ----------
    /// 侧栏开关。Windows 上标题栏整条是系统拖拽区，里面的按钮收不到点击，
    /// 所以开关放在侧栏头部（展开）和左侧窄栏（收起）。
    fn sidebar_toggle(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(id)
            .size(px(28.))
            .flex_shrink_0()
            .rounded_md()
            .flex()
            .justify_center()
            .items_center()
            .cursor_pointer()
            .hover(|d| d.bg(rgb(SAND_ACTIVE)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .child(Icon::new(IconAll::PanelLeft).size_4().text_color(rgb(INK_SOFT)))
    }

    /// 权限模式弹层：锚在输入卡片左下，向上展开。
    fn render_mode_menu(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut menu = div()
            .id("mode-menu")
            .occlude()
            .absolute()
            .left(px(44.))
            .bottom(px(104.))
            .w(px(300.))
            .v_flex()
            .p_1()
            .gap_0p5()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .rounded_lg()
            .shadow_md();
        for (i, (name, desc, icon, _)) in PERMISSION_MODES.iter().enumerate() {
            let active = self.permission_mode == i;
            menu = menu.child(
                div()
                    .id(("mode-item", i))
                    .h_flex()
                    .items_center()
                    .gap_2p5()
                    .px_2p5()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(SAND)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.permission_mode = i;
                        this.mode_menu_open = false;
                        // 已连接内核时同步到会话（ACP session/set_mode）
                        if this.kernel_ready {
                            if let Some(kernel) = &this.kernel {
                                let _ = kernel.set_mode(PERMISSION_MODES[i].3);
                            }
                        }
                        cx.notify();
                    }))
                    .child(Icon::new(*icon).size_4().text_color(rgb(INK_SOFT)))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(rgb(INK))
                                    .child(*name),
                            )
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child(*desc),
                            ),
                    )
                    .when(active, |d| {
                        d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                    }),
            );
        }
        menu
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> Div {
        let send_bg = if self.busy { rgb(INK_SOFT) } else { rgb(CORAL) };
        let (mode_name, _, mode_icon, _) = PERMISSION_MODES[self.permission_mode];
        let card = div()
            .id("composer")
            .relative()
            .v_flex()
            .w_full()
            .gap_2()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .rounded(px(22.))
            .shadow_sm()
            .px_4()
            .pt_3()
            .pb_3()
            .child(Input::new(&self.input).appearance(false).bordered(false))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(30.))
                            .flex_shrink_0()
                            .rounded_full()
                            .border_1()
                            .border_color(rgb(LINE))
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(Icon::new(IconAll::Plus).size_4().text_color(rgb(INK_SOFT))),
                    )
                    .child(
                        div()
                            .id("mode-chip")
                            .h_flex()
                            .gap_1p5()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .when(self.mode_menu_open, |d| d.bg(rgb(SAND)))
                            .hover(|d| d.bg(rgb(SAND)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mode_menu_open = !this.mode_menu_open;
                                cx.notify();
                            }))
                            .child(
                                Icon::new(mode_icon)
                                    .size_3p5()
                                    .text_color(rgb(INK_SOFT)),
                            )
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child(mode_name),
                            )
                            .child(
                                Icon::new(IconAll::ChevronDown)
                                    .size_3()
                                    .text_color(rgb(INK_SOFT)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .h_flex()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child("GLM-5.3"),
                            )
                            .child(
                                Icon::new(IconAll::ChevronDown)
                                    .size_3()
                                    .text_color(rgb(INK_SOFT)),
                            ),
                    )
                    .child(
                        div()
                            .id("send")
                            .size(px(32.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(send_bg)
                            .flex()
                            .justify_center()
                            .items_center()
                            .cursor_pointer()
                            .child(
                                Icon::new(if self.busy {
                                    IconAll::CircleStop
                                } else {
                                    IconAll::ArrowUp
                                })
                                .size_4()
                                .text_color(rgb(0xFFFFFF)),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.busy {
                                    this.stop(cx);
                                } else {
                                    this.send(window, cx);
                                }
                            })),
                    ),
            )
            .when(self.mode_menu_open, |d| d.child(self.render_mode_menu(cx)));

        div()
            .v_flex()
            .max_w(px(760.))
            .mx_auto()
            .w_full()
            .gap_2p5()
            .child(card)
            .child(
                // 卡片下方：环境 + 项目
                div()
                    .h_flex()
                    .gap_2()
                    .child(Self::chip(IconAll::Monitor, "Local"))
                    .child(Self::chip(IconAll::Folder, "claudesktop")),
            )
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
        if self.follow_scroll {
            self.scroll.scroll_to_bottom();
        }

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

        let main = if self.selected.is_none() {
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
            for (idx, item) in self.transcript.iter().enumerate() {
                list = list.child(self.render_item(idx, item, cx));
            }

            let scroll = div()
                .id("transcript")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
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

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);

            let mut options = TitleBar::window_options();
            options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1280.), px(820.)),
                cx,
            )));
            if let Some(tb) = options.titlebar.as_mut() {
                tb.title = Some("claudesktop · M1 UI 预览".into());
            }

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    let app = ChatApp::new(window, cx);
                    // 延迟 1.5s 触发自动演示，避开启动阶段的窗口初始化。
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(1500))
                            .await;
                        let _ = this.update(cx, |app, cx| app.autodemo_hook(cx));
                    })
                    .detach();
                    app
                })
            })
            .expect("Failed to open window");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn tool_title_split() {
        let (n, a) = split_tool_title("Read /a/b.rs");
        assert_eq!((n.as_ref(), a.as_ref()), ("Read", "/a/b.rs"));
        let (n, a) = split_tool_title("`cargo build -p x`");
        assert_eq!((n.as_ref(), a.as_ref()), ("Bash", "cargo build -p x"));
        let (n, a) = split_tool_title("Edit");
        assert_eq!((n.as_ref(), a.as_ref()), ("Edit", ""));
        let (n, a) = split_tool_title("读取 file.mjs");
        assert_eq!((n.as_ref(), a.as_ref()), ("Tool", "读取 file.mjs"));
    }

    #[core::prelude::v1::test]
    fn output_summary() {
        assert_eq!(summarize_output("\"a\nb  c\""), "a b c");
        let long = "x".repeat(300);
        let out = summarize_output(&long);
        assert_eq!(out.chars().count(), 141);
        assert!(out.ends_with('…'));
    }

    #[core::prelude::v1::test]
    fn tool_phase_mapping() {
        assert!(tool_phase("completed") == ToolPhase::Done);
        assert!(tool_phase("failed") == ToolPhase::Failed);
        assert!(tool_phase("in_progress") == ToolPhase::Running);
    }
}
