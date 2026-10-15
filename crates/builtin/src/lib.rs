//! # claudesktop-builtin
//!
//! 自研内核（进程内）：直接调 LLM provider，产出与 ACP 内核同一套
//! [`KernelEvent`]，UI 无感切换。
//!
//! - OpenAI 兼容 provider：流式文本 / 思考 / 用量 / 工具调用
//! - 工具循环：模型请求工具 → 按模式与规则判断权限（必要时发 `PermissionRequest`
//!   等 UI 应答）→ 执行 → 结果回给模型，直到模型不再调用工具
//! - 模式：Manual / Accept edits / Plan（只读）/ Bypass；「总是允许」规则按项目落盘
//! - 续接：用本地会话日志重建上下文（见 `history` 模块）
//! - 项目说明：AGENTS.md / CLAUDE.md 读进系统提示词（见 `instructions` 模块）
//! - 上下文压缩：快满时 / 超限报错后把较早的对话总结成摘要（见 `compact` 模块）
//!
//! 线程模型：每轮对话一个线程（阻塞读 SSE 流、执行工具、等权限应答）；
//! 取消 = 置该轮的标志位，各阻塞点轮询检查，取消后不再发出任何事件。

mod anthropic;
mod compact;
mod fetch;
mod history;
mod instructions;
mod openai;
mod permissions;
mod provider;
mod sse;
mod tools;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;
pub use permissions::{PermissionRules, default_rules_path};
pub use provider::{
    ChatMessage, ChatProvider, ContentPart, FunctionCall, ModelInfo, Protocol, ProviderConfig,
    RequestParams, StreamEvent, StreamOutcome, ToolCall, Usage,
};

/// 列出端点提供的模型（阻塞，设置页在后台线程调用）。
pub fn fetch_models(config: ProviderConfig) -> anyhow::Result<Vec<ModelInfo>> {
    make_provider(&config).list_models()
}

/// 按配置里的协议构造 provider。
fn make_provider(config: &ProviderConfig) -> std::sync::Arc<dyn ChatProvider> {
    match config.protocol {
        Protocol::OpenAiCompat => std::sync::Arc::new(OpenAiProvider::new(config.clone())),
        Protocol::Anthropic => std::sync::Arc::new(AnthropicProvider::new(config.clone())),
    }
}

use claudesktop_core::{
    EventSink, Kernel, KernelEvent, ModeView, PermissionOptionView, Record, RoutedEvent,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tools::{Access, Action, ToolOutput};

/// 一轮里最多请求模型的次数（防止工具循环停不下来）
const MAX_STEPS: usize = 50;
/// 同一步里最多同时执行几个只读工具
const PARALLEL_TOOLS: usize = 8;

const MODE_DEFAULT: &str = "default";
const MODE_ACCEPT_EDITS: &str = "acceptEdits";
const MODE_PLAN: &str = "plan";
const MODE_BYPASS: &str = "bypassPermissions";

#[derive(Debug, Clone)]
pub struct BuiltinConfig {
    pub provider: ProviderConfig,
    /// 默认工作目录（写进系统提示词，工具的相对路径以此为准）
    pub cwd: PathBuf,
    /// 「总是允许」规则文件；None = 只在内存里
    pub rules_path: Option<PathBuf>,
    /// 用户级说明文件（默认 `~/.claude/CLAUDE.md`），和项目里的 AGENTS.md / CLAUDE.md
    /// 一起进系统提示词；None = 不读
    pub user_instructions: Option<PathBuf>,
    /// 端点报告的上下文窗口（模型 ID → token）：用量快到窗口时自动压缩；
    /// 查不到窗口的模型只在端点报「超出上下文」后压缩
    pub context_windows: HashMap<String, u64>,
}

impl BuiltinConfig {
    /// 规则文件放在默认数据目录。
    pub fn new(provider: ProviderConfig, cwd: PathBuf) -> Self {
        Self {
            provider,
            cwd,
            rules_path: default_rules_path(),
            user_instructions: instructions::default_user_file(),
            context_windows: HashMap::new(),
        }
    }

    /// 从环境变量读取：
    /// - `CLAUDESKTOP_BASE_URL`（必填）：端点，如 `http://127.0.0.1:8045/v1`
    /// - `CLAUDESKTOP_MODEL`（必填）：模型 ID
    /// - `CLAUDESKTOP_API_KEY`（可选，其次 `OPENAI_API_KEY`）
    /// - `CLAUDEDESKTOP_PROTOCOL`（可选，`anthropic` = Anthropic 原生协议）
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let base_url = var("CLAUDESKTOP_BASE_URL")
            .ok_or_else(|| anyhow::anyhow!("未设置 CLAUDESKTOP_BASE_URL（OpenAI 兼容端点）"))?;
        let model =
            var("CLAUDESKTOP_MODEL").ok_or_else(|| anyhow::anyhow!("未设置 CLAUDESKTOP_MODEL"))?;
        let protocol = match var("CLAUDEDESKTOP_PROTOCOL").as_deref() {
            Some("anthropic") => Protocol::Anthropic,
            _ => Protocol::OpenAiCompat,
        };
        Ok(Self::new(
            ProviderConfig {
                protocol,
                base_url,
                api_key: var("CLAUDESKTOP_API_KEY").or_else(|| var("OPENAI_API_KEY")),
                model,
                params: RequestParams::default(),
            },
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ))
    }

    /// 写进会话元信息的内核标识（见 [`ProviderConfig::identity`]）。
    pub fn identity(&self) -> String {
        self.provider.identity()
    }
}

fn modes() -> Vec<ModeView> {
    let m = |id: &str, name: &str, desc: &str| ModeView {
        id: id.into(),
        name: name.into(),
        description: Some(desc.into()),
    };
    vec![
        m(MODE_DEFAULT, "Manual", "写文件和执行命令前先询问"),
        m(MODE_ACCEPT_EDITS, "Accept edits", "自动允许项目内的文件编辑，命令仍询问"),
        m(MODE_PLAN, "Plan", "只读：不写文件、不执行命令"),
        m(MODE_BYPASS, "Bypass", "全部自动允许（谨慎）"),
    ]
}

struct SessionState {
    cwd: PathBuf,
    /// 本会话使用的模型
    model: String,
    /// 不含系统提示词的对话历史
    history: Vec<ChatMessage>,
    mode: String,
    /// 历史是从本地日志重建的（工具调用只剩摘要）
    restored: bool,
    /// 用户级和项目里的说明（每轮开始时重读）
    instructions: Vec<instructions::Instruction>,
    /// 较早对话的摘要（压缩过才有，进系统提示词）
    summary: Option<String>,
    /// 端点最近一次报告的上下文大小：(token, 当时覆盖到的历史条数)；压缩后清空
    measured: Option<(u64, usize)>,
    /// 进行中的一轮的取消标志
    turn: Option<Arc<AtomicBool>>,
}

impl SessionState {
    fn system_prompt(&self) -> String {
        system_prompt(
            &self.cwd,
            &self.mode,
            self.restored,
            &self.instructions,
            self.summary.as_deref(),
        )
    }

    /// 系统提示词 + 历史的粗估 token（端点还没报告过用量时用）。
    fn estimate_tokens(&self) -> u64 {
        self.system_prompt().len() as u64 / 4 + compact::estimate_tokens(&self.history)
    }
}

struct Inner {
    provider: Arc<dyn ChatProvider>,
    /// 新会话的默认模型（最近一次 `set_model` 选的，初始为配置里的）
    default_model: Mutex<String>,
    sink: EventSink,
    agent_name: String,
    default_cwd: PathBuf,
    user_instructions: Option<PathBuf>,
    /// 见 [`BuiltinConfig::context_windows`]；界面取到模型列表后经 `set_context_windows` 更新
    context_windows: Mutex<HashMap<String, u64>>,
    sessions: Mutex<HashMap<String, SessionState>>,
    next_session: AtomicU64,
    rules: Mutex<PermissionRules>,
    /// 等待 UI 应答的权限请求：请求 ID → 应答通道
    pending: Mutex<HashMap<u64, mpsc::Sender<Option<String>>>>,
    next_request: AtomicU64,
}

/// 权限判定结果
enum Verdict {
    Allow,
    /// 拒绝；内容回给模型
    Deny(String),
}

impl Inner {
    fn emit(&self, session: Option<&str>, event: KernelEvent) {
        (self.sink)(RoutedEvent { session: session.map(str::to_owned), event });
    }

    /// 本步要发给模型的消息、当前模式、工作目录与模型；会话已不存在返回 None。
    fn snapshot(&self, session: &str) -> Option<(Vec<ChatMessage>, String, PathBuf, String)> {
        let sessions = self.sessions.lock().unwrap();
        let state = sessions.get(session)?;
        let mut messages = vec![ChatMessage::new("system", state.system_prompt())];
        messages.extend(state.history.iter().cloned());
        Some((messages, state.mode.clone(), state.cwd.clone(), state.model.clone()))
    }

    /// 模型的上下文窗口；对不上完整 ID 时按最后一段找（`provider/name` → `name`）。
    fn window(&self, model: &str) -> Option<u64> {
        let name = |id: &str| id.rsplit('/').next().unwrap_or(id).to_string();
        let windows = self.context_windows.lock().unwrap();
        windows
            .get(model)
            .copied()
            .or_else(|| windows.iter().find(|(id, _)| name(id) == name(model)).map(|(_, n)| *n))
            .filter(|w| *w > 0)
    }

    /// 下一次请求大约要带的 token：端点上次报告的 + 之后新加的消息（粗估）；没报告过就整个粗估。
    fn context_estimate(&self, session: &str) -> u64 {
        let sessions = self.sessions.lock().unwrap();
        let Some(state) = sessions.get(session) else {
            return 0;
        };
        match state.measured {
            Some((tokens, covered)) if covered <= state.history.len() => {
                tokens + compact::estimate_tokens(&state.history[covered..])
            }
            _ => state.estimate_tokens(),
        }
    }

    /// 记下端点报告的上下文大小：历史里除了最后 `unmeasured` 条，都算在 `tokens` 里。
    fn measure(&self, session: &str, tokens: u64, unmeasured: usize) {
        if tokens == 0 {
            return;
        }
        if let Some(state) = self.sessions.lock().unwrap().get_mut(session) {
            state.measured = Some((tokens, state.history.len().saturating_sub(unmeasured)));
        }
    }

    /// 压缩上下文：挑切点、请模型总结、换掉历史（规则见 `compact` 模块）。
    /// `idle` = 手动压缩（没有进行中的一轮），全部进摘要；`keep_budget` 是保留尾部的 token 预算。
    /// 开始和结果都报给 UI（`Compacting` → `Compacted` / `CompactFailed`）。
    /// 没有可压缩的内容返回 `Ok(None)`，成功返回总结请求的用量，被取消或失败返回 `Err`。
    fn compact(
        &self,
        session: &str,
        flag: &AtomicBool,
        model: &str,
        idle: bool,
        keep_budget: u64,
        transcript_bytes: usize,
    ) -> anyhow::Result<Option<Usage>> {
        let stopped = || flag.load(Ordering::SeqCst);
        let (history, previous) = {
            let sessions = self.sessions.lock().unwrap();
            let state = sessions.get(session).ok_or_else(|| anyhow::anyhow!("会话已关闭"))?;
            (state.history.clone(), state.summary.clone())
        };
        let Some(split) = compact::split(&history, keep_budget, idle) else {
            return Ok(None);
        };
        self.emit(Some(session), KernelEvent::Compacting);
        let result = self.summarize(
            session,
            model,
            &history[..split.tail_from],
            previous.as_deref(),
            transcript_bytes,
            &stopped,
        );
        if stopped() {
            anyhow::bail!("cancelled");
        }
        let (summary, usage) = match result {
            Ok(r) => r,
            Err(err) => {
                let message = format!("{err:#}");
                self.emit(Some(session), KernelEvent::CompactFailed { message });
                return Err(err);
            }
        };
        // 和 commit() 一样持锁判断取消；一轮之内历史只有本线程在改，切点仍然有效
        let context_tokens = {
            let mut sessions = self.sessions.lock().unwrap();
            let state = match sessions.get_mut(session) {
                Some(state) if !stopped() => state,
                _ => anyhow::bail!("cancelled"),
            };
            state.history = split.apply(&history);
            state.summary = Some(summary.clone());
            state.measured = None;
            state.estimate_tokens()
        };
        self.emit(Some(session), KernelEvent::Compacted { summary, context_tokens });
        Ok(Some(usage.unwrap_or_default()))
    }

    /// 请模型把一段对话写成摘要（不带工具）。记录太长、总结请求本身也超限时，记录减半再试，最多两次。
    fn summarize(
        &self,
        session: &str,
        model: &str,
        head: &[ChatMessage],
        previous: Option<&str>,
        mut max_bytes: usize,
        stopped: &dyn Fn() -> bool,
    ) -> anyhow::Result<(String, Option<Usage>)> {
        let mut attempt = 0;
        loop {
            let request = compact::summary_request(previous, &compact::transcript(head, max_bytes));
            let mut text = String::new();
            let result =
                self.provider.stream_chat(
                    model,
                    &request,
                    &[],
                    stopped,
                    &mut |event| match event {
                        StreamEvent::Text(t) => text.push_str(&t),
                        StreamEvent::Reasoning(_) => {}
                        StreamEvent::Retrying { status, attempt, delay_ms, reason } => self.emit(
                            Some(session),
                            KernelEvent::Retrying { status, attempt, delay_ms, reason },
                        ),
                    },
                );
            match result {
                Err(err)
                    if attempt < 2
                        && claudesktop_core::is_context_overflow(&format!("{err:#}")) =>
                {
                    attempt += 1;
                    max_bytes /= 2;
                }
                Err(err) => return Err(err),
                Ok(outcome) => {
                    let text = text.trim().to_string();
                    anyhow::ensure!(!text.is_empty(), "模型没有返回摘要");
                    return Ok((text, outcome.usage));
                }
            }
        }
    }

    fn model_of(&self, session: &str) -> Option<String> {
        self.sessions.lock().unwrap().get(session).map(|s| s.model.clone())
    }

    fn mode(&self, session: &str) -> String {
        self.sessions
            .lock()
            .unwrap()
            .get(session)
            .map_or_else(|| MODE_DEFAULT.into(), |s| s.mode.clone())
    }

    /// 把一步的消息写进历史。和 `cancel()` 共用锁判断标志位：
    /// 取消之后（新一轮可能已经追加了用户消息）不再写，返回 false。
    fn commit(&self, session: &str, flag: &AtomicBool, messages: Vec<ChatMessage>) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get_mut(session) {
            Some(state) if !flag.load(Ordering::SeqCst) => {
                state.history.extend(messages);
                true
            }
            _ => false,
        }
    }

    /// 本轮结束：清掉自己的标志（取消后可能已经开始了新的一轮）。
    fn finish(&self, session: &str, flag: &Arc<AtomicBool>) {
        if let Some(state) = self.sessions.lock().unwrap().get_mut(session)
            && state.turn.as_ref().is_some_and(|f| Arc::ptr_eq(f, flag))
        {
            state.turn = None;
        }
    }

    /// 执行一次工具调用（含权限判定），并把进度报给 UI。
    fn run_tool(
        &self,
        session: &str,
        cwd: &Path,
        call: &ToolCall,
        stopped: &dyn Fn() -> bool,
    ) -> ToolOutput {
        // 待办清单不碰文件、不用授权，也不占一行工具：直接换掉界面上的计划
        if call.function.name == tools::TODO_WRITE {
            return match tools::parse_todos(&call.function.arguments) {
                Ok(entries) => {
                    let summary = tools::todo_summary(&entries);
                    self.emit(Some(session), KernelEvent::PlanReplaced { entries });
                    ToolOutput::ok(format!("Todo list updated: {summary}"), summary)
                }
                Err(message) => ToolOutput::fail(message, "参数无效"),
            };
        }
        let (action, title) = self.start_tool(session, cwd, call);
        self.finish_tool(session, cwd, call, action, &title, stopped)
    }

    /// 一步里的全部调用：连续的只读调用一起并行跑，其余依次执行；结果按调用顺序返回。
    /// 中途被取消时返回 None。
    fn run_tools(
        &self,
        session: &str,
        cwd: &Path,
        calls: &[ToolCall],
        stopped: &(dyn Fn() -> bool + Sync),
    ) -> Option<Vec<ToolOutput>> {
        let mut outputs = Vec::with_capacity(calls.len());
        let mut rest = calls;
        while !rest.is_empty() {
            let n = rest
                .iter()
                .take_while(|c| tools::parallel_safe(&c.function.name))
                .count()
                .min(PARALLEL_TOOLS);
            if n < 2 {
                outputs.push(self.run_tool(session, cwd, &rest[0], stopped));
                rest = &rest[1..];
            } else {
                let (group, tail) = rest.split_at(n);
                // 先按顺序报出全部工具行，界面上的顺序和模型的调用顺序一致
                let started: Vec<_> =
                    group.iter().map(|c| self.start_tool(session, cwd, c)).collect();
                outputs.extend(std::thread::scope(|s| {
                    let handles: Vec<_> = group
                        .iter()
                        .zip(started)
                        .map(|(call, (action, title))| {
                            s.spawn(move || {
                                self.finish_tool(session, cwd, call, action, &title, stopped)
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| {
                            h.join().unwrap_or_else(|_| ToolOutput::fail("tool crashed", "出错"))
                        })
                        .collect::<Vec<_>>()
                }));
                rest = tail;
            }
            if stopped() {
                return None;
            }
        }
        Some(outputs)
    }

    /// 解析调用并报出工具行（改动类先带上 diff 预览）。
    fn start_tool(
        &self,
        session: &str,
        cwd: &Path,
        call: &ToolCall,
    ) -> (Result<Action, String>, String) {
        let action = Action::parse(&call.function.name, &call.function.arguments, cwd);
        let (title, kind) = match &action {
            Ok(a) => (a.title(cwd), a.kind()),
            Err(_) => (call.function.name.clone(), "other"),
        };
        self.emit(
            Some(session),
            KernelEvent::ToolCallStarted {
                id: call.id.clone(),
                title: title.clone(),
                status: "pending".into(),
                kind: kind.into(),
            },
        );
        // 改动预览：先于权限请求发出，用户能看着 diff 决定是否放行
        if let Some(diff) = action.as_ref().ok().and_then(|a| a.preview(cwd)) {
            self.emit(Some(session), KernelEvent::ToolCallDiff { id: call.id.clone(), diff });
        }
        (action, title)
    }

    /// 授权、执行，并报告结果。
    fn finish_tool(
        &self,
        session: &str,
        cwd: &Path,
        call: &ToolCall,
        action: Result<Action, String>,
        title: &str,
        stopped: &dyn Fn() -> bool,
    ) -> ToolOutput {
        let output = match action {
            Err(message) => ToolOutput::fail(message, "参数无效"),
            Ok(action) => match self.authorize(session, cwd, &action, title, stopped) {
                Verdict::Allow => {
                    if stopped() {
                        return ToolOutput::fail("cancelled", "已中断");
                    }
                    self.emit(
                        Some(session),
                        KernelEvent::ToolCallUpdated {
                            id: call.id.clone(),
                            title: None,
                            status: Some("in_progress".into()),
                            raw_output: None,
                            output: None,
                        },
                    );
                    action.run(stopped)
                }
                Verdict::Deny(reason) => ToolOutput::fail(reason, "已拒绝"),
            },
        };
        if !stopped() {
            self.emit(
                Some(session),
                KernelEvent::ToolCallUpdated {
                    id: call.id.clone(),
                    title: None,
                    status: Some(if output.ok { "completed" } else { "failed" }.into()),
                    raw_output: Some(output.summary.clone()),
                    output: Some(claudesktop_core::output_preview(&output.content)),
                },
            );
        }
        output
    }

    /// 读工具直接放行；其余按模式 → 已保存规则 → 询问用户。
    /// 抓网页是只读的，Plan 模式下也能用（照样要问）。
    fn authorize(
        &self,
        session: &str,
        cwd: &Path,
        action: &Action,
        title: &str,
        stopped: &dyn Fn() -> bool,
    ) -> Verdict {
        let access = action.access();
        if access == Access::Read {
            return Verdict::Allow;
        }
        let mode = self.mode(session);
        match mode.as_str() {
            MODE_PLAN if access != Access::Fetch => {
                return Verdict::Deny(
                    "Plan mode is active: writing files and running commands are not allowed. \
                     Investigate with read-only tools and present a plan instead."
                        .into(),
                );
            }
            MODE_BYPASS => return Verdict::Allow,
            MODE_ACCEPT_EDITS if access == Access::Edit && action.inside(cwd) => {
                return Verdict::Allow;
            }
            _ => {}
        }
        let rule = action.rule(cwd);
        let command = match action {
            Action::Shell { command, .. } => Some(command.as_str()),
            _ => None,
        };
        if let Some(rule) = &rule
            && self.rules.lock().unwrap().allows(cwd, rule, command)
        {
            return Verdict::Allow;
        }

        let option =
            |id: &str, name: String| PermissionOptionView { id: id.into(), name, kind: id.into() };
        let mut options = vec![option("allow_once", "允许一次".into())];
        if let Some(rule) = &rule {
            let name = if let Some(prefix) = rule.strip_prefix("shell:") {
                format!("总是允许 `{prefix}` 开头的命令")
            } else if let Some(host) = rule.strip_prefix("fetch:") {
                format!("本项目内总是允许访问 {host}")
            } else {
                "本项目内总是允许编辑".into()
            };
            options.push(option("allow_always", name));
        }
        options.push(option("reject_once", "拒绝".into()));

        match self.ask(session, title, options, stopped).as_deref() {
            Some("allow_once") => Verdict::Allow,
            Some("allow_always") => {
                if let Some(rule) = rule
                    && let Err(e) = self.rules.lock().unwrap().add(cwd, rule)
                {
                    self.emit(None, KernelEvent::Status(format!("保存权限规则失败: {e}")));
                }
                Verdict::Allow
            }
            _ => Verdict::Deny(
                "The user denied this tool call. Do not retry it; \
                 stop and ask the user how they would like to proceed."
                    .into(),
            ),
        }
    }

    /// 发出权限请求并阻塞等待应答；被取消时返回 None。
    fn ask(
        &self,
        session: &str,
        title: &str,
        options: Vec<PermissionOptionView>,
        stopped: &dyn Fn() -> bool,
    ) -> Option<String> {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.emit(
            Some(session),
            KernelEvent::PermissionRequest { id, title: title.to_string(), options },
        );
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(choice) => return choice,
                Err(mpsc::RecvTimeoutError::Timeout) if !stopped() => {}
                Err(_) => {
                    self.pending.lock().unwrap().remove(&id);
                    return None;
                }
            }
        }
    }
}

pub struct BuiltinKernel {
    inner: Arc<Inner>,
}

impl BuiltinKernel {
    /// 创建内核。进程内无握手，立即报告 `Connected`。
    pub fn spawn(config: BuiltinConfig, sink: EventSink) -> anyhow::Result<Self> {
        let agent_name = format!("builtin · {}", config.provider.model);
        let inner = Arc::new(Inner {
            provider: make_provider(&config.provider),
            default_model: Mutex::new(config.provider.model.clone()),
            sink,
            agent_name: agent_name.clone(),
            default_cwd: config.cwd,
            user_instructions: config.user_instructions,
            context_windows: Mutex::new(config.context_windows),
            sessions: Mutex::default(),
            next_session: AtomicU64::new(1),
            rules: Mutex::new(PermissionRules::load(config.rules_path)),
            pending: Mutex::default(),
            next_request: AtomicU64::new(1),
        });
        inner.emit(None, KernelEvent::Status(format!("自研内核: {}", config.provider.base_url)));
        inner.emit(None, KernelEvent::Connected { agent_name });
        Ok(Self { inner })
    }

    /// 新建会话（`history` 有内容 = 续接），报告 `Started`。
    fn open_session(&self, token: u64, cwd: Option<PathBuf>, history: history::Rebuilt) {
        let n = self.inner.next_session.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("builtin-{}-{n}", std::process::id());
        let cwd = cwd.unwrap_or_else(|| self.inner.default_cwd.clone());
        let resumed = !history.messages.is_empty() || history.summary.is_some();
        let instructions = instructions::load(&cwd, self.inner.user_instructions.as_deref());
        let model = self.inner.default_model.lock().unwrap().clone();
        self.inner.sessions.lock().unwrap().insert(
            session_id.clone(),
            SessionState {
                cwd,
                model: model.clone(),
                history: history.messages,
                mode: MODE_DEFAULT.into(),
                restored: resumed,
                instructions,
                summary: history.summary,
                measured: None,
                turn: None,
            },
        );
        self.inner.emit(
            Some(&session_id),
            KernelEvent::Started {
                token,
                agent_name: self.inner.agent_name.clone(),
                session_id: session_id.clone(),
                model: Some(model),
                modes: modes(),
                current_mode: Some(MODE_DEFAULT.into()),
                resumed,
            },
        );
    }
}

fn system_prompt(
    cwd: &Path,
    mode: &str,
    restored: bool,
    project: &[instructions::Instruction],
    summary: Option<&str>,
) -> String {
    let mut prompt = format!(
        "You are a coding agent running inside claudesktop, a desktop agent harness, \
         working on the user's machine.\n\
         Working directory: {}\n\
         Platform: {}\n\n\
         Use the tools to inspect and change the project: find files with glob and search code with grep \
         (rather than shell commands), read files before editing them, \
         request independent reads and searches together in one step (they run in parallel), \
         prefer edit_file for small changes, read URLs the user gives with web_fetch, and verify your work (e.g. build or run tests) \
         when it makes sense. For tasks with several steps, track progress with todo_write. \
         Tool calls that change things may need the user's approval; \
         if one is denied, do not retry it and ask the user instead.\n\
         Keep replies concise. Answer in the user's language. Use Markdown for structure and code.",
        cwd.display(),
        std::env::consts::OS,
    );
    if restored {
        prompt.push_str(
            "\n\nThis conversation was restored from a saved log. Earlier tool calls appear as \
             `[tool] <title> → <status>: <summary>` lines; their full inputs and outputs are not \
             available, so re-read files if you need details. Never write such lines yourself.",
        );
    }
    if let Some(section) = instructions::prompt_section(project) {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
    if let Some(summary) = summary {
        prompt.push_str("\n\n");
        prompt.push_str(&compact::prompt_section(summary));
    }
    if mode == MODE_PLAN {
        prompt.push_str(
            "\n\nPlan mode is active: only read-only tools are available. \
             Investigate, then present a concrete step-by-step plan for the user to approve. \
             Do not attempt to modify anything.",
        );
    }
    prompt
}

/// provider 的 finish_reason → ACP 风格的 stop_reason。
fn stop_reason(finish: Option<&str>) -> String {
    match finish {
        None | Some("stop") | Some("tool_calls") => "end_turn".into(),
        Some("length") => "max_tokens".into(),
        Some(other) => other.into(),
    }
}

/// 交给模型总结的记录最多多少字节：窗口的一部分（按 4 字节 / token 估），窗口未知时用固定上限。
fn transcript_budget(window: Option<u64>) -> usize {
    window.map_or(compact::TRANSCRIPT_MAX_BYTES_UNKNOWN, |w| {
        (w * compact::TRANSCRIPT_PERCENT / 100 * 4) as usize
    })
}

/// 一轮对话（在独立线程里运行）：请求模型 → 执行工具 → 再请求，直到不再调用工具。
fn run_turn(inner: Arc<Inner>, session: String, flag: Arc<AtomicBool>) {
    let stopped = || flag.load(Ordering::SeqCst);
    let mut usage: Option<Usage> = None;
    // 最后一次请求的输入 + 输出 = 此刻上下文的大小（之前各步都包含在这次的输入里）
    let mut context_tokens = 0;
    let mut stop = "max_turn_requests".to_string();
    // 上一步刚压缩过（不连着压两次）/ 这一轮已经因为超限报错压缩过（只压一次）
    let mut just_compacted = false;
    let mut overflow_compacted = false;

    for _ in 0..MAX_STEPS {
        // 用量快到窗口：先压缩再请求
        if !just_compacted {
            if let Some((model, window)) =
                inner.model_of(&session).and_then(|m| inner.window(&m).map(|w| (m, w)))
                && inner.context_estimate(&session) * 100 >= window * compact::AUTO_COMPACT_PERCENT
            {
                match inner.compact(
                    &session,
                    &flag,
                    &model,
                    false,
                    window * compact::KEEP_PERCENT / 100,
                    transcript_budget(Some(window)),
                ) {
                    Ok(Some(u)) => usage.get_or_insert_default().add(u),
                    Ok(None) => {}
                    // 压缩失败（已报给界面）就带着原样的上下文接着请求
                    Err(_) if !stopped() => {}
                    Err(_) => return,
                }
                just_compacted = true;
            }
        } else {
            just_compacted = false;
        }
        let Some((messages, mode, cwd, model)) = inner.snapshot(&session) else {
            return;
        };
        let specs = tools::specs(mode != MODE_PLAN);
        let mut text = String::new();
        let result =
            inner.provider.stream_chat(&model, &messages, &specs, &stopped, &mut |event| {
                if stopped() {
                    return;
                }
                match event {
                    StreamEvent::Text(t) => {
                        text.push_str(&t);
                        inner.emit(Some(&session), KernelEvent::AgentMessageChunk { text: t });
                    }
                    StreamEvent::Reasoning(t) => {
                        inner.emit(Some(&session), KernelEvent::AgentThoughtChunk { text: t })
                    }
                    StreamEvent::Retrying { status, attempt, delay_ms, reason } => inner.emit(
                        Some(&session),
                        KernelEvent::Retrying { status, attempt, delay_ms, reason },
                    ),
                }
            });
        if stopped() {
            return; // cancel() 已经报告过 TurnEnded
        }
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(err) => {
                let mut message = format!("{err:#}");
                // 超出上下文：压缩一次再重发这一步
                if !overflow_compacted && claudesktop_core::is_context_overflow(&message) {
                    overflow_compacted = true;
                    let window = inner.window(&model);
                    let keep = window.unwrap_or_else(|| compact::estimate_tokens(&messages))
                        * compact::KEEP_PERCENT
                        / 100;
                    match inner.compact(
                        &session,
                        &flag,
                        &model,
                        false,
                        keep,
                        transcript_budget(window),
                    ) {
                        Ok(Some(u)) => {
                            usage.get_or_insert_default().add(u);
                            just_compacted = true;
                            continue;
                        }
                        Ok(None) => {}
                        Err(_) if stopped() => return,
                        Err(e) => message.push_str(&format!("\n（自动压缩上下文也失败了：{e:#}）")),
                    }
                }
                // 和从日志重建的历史一致（见 history::rebuild）：失败也占一条 assistant 消息，
                // 用户点「重试」重发时不会出现两条连着的用户消息（Anthropic 要求交替）
                inner.commit(
                    &session,
                    &flag,
                    vec![ChatMessage::new("assistant", format!("[turn failed: {message}]"))],
                );
                inner.finish(&session, &flag);
                inner.emit(Some(&session), KernelEvent::PromptFailed { message });
                return;
            }
        };
        let mut step_tokens = 0;
        if let Some(u) = outcome.usage {
            usage.get_or_insert_default().add(u);
            step_tokens = u.input_tokens + u.output_tokens;
            context_tokens = step_tokens;
        }

        if outcome.tool_calls.is_empty() {
            if !text.is_empty() {
                inner.commit(&session, &flag, vec![ChatMessage::new("assistant", text)]);
            }
            inner.measure(&session, step_tokens, 0);
            stop = stop_reason(outcome.finish_reason.as_deref());
            break;
        }

        // 同一步的多个调用执行完（连续的只读调用并行）；assistant 消息和全部结果一起写进历史，
        // 保证 tool_calls 后面总跟着对应的 tool 消息
        let Some(outputs) = inner.run_tools(&session, &cwd, &outcome.tool_calls, &stopped) else {
            return;
        };
        let results: Vec<_> = outcome
            .tool_calls
            .iter()
            .zip(outputs)
            .map(|(call, output)| ChatMessage::tool_result(call.id.clone(), output.content))
            .collect();
        let mut step = Vec::with_capacity(results.len() + 1);
        let unmeasured = results.len();
        step.push(ChatMessage::assistant_tool_calls(text, outcome.tool_calls));
        step.extend(results);
        if !inner.commit(&session, &flag, step) {
            return;
        }
        // 工具结果还没被端点数过
        inner.measure(&session, step_tokens, unmeasured);
    }

    inner.finish(&session, &flag);
    if let Some(u) = usage {
        inner.emit(
            Some(&session),
            KernelEvent::UsageUpdated {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                reasoning_tokens: u.reasoning_tokens,
                context_tokens,
                cache_read_tokens: u.cache_read_tokens,
            },
        );
    }
    inner.emit(Some(&session), KernelEvent::TurnEnded { stop_reason: stop });
}

impl Kernel for BuiltinKernel {
    fn kind(&self) -> &'static str {
        "builtin"
    }

    fn label(&self) -> String {
        self.inner.agent_name.clone()
    }

    fn new_session(&self, token: u64, cwd: Option<PathBuf>) -> anyhow::Result<()> {
        self.open_session(token, cwd, history::Rebuilt::default());
        Ok(())
    }

    /// 进程内的会话不跨进程存活：`session_id` 无用，上下文从本地日志重建。
    fn resume_session(
        &self,
        token: u64,
        _session_id: &str,
        cwd: Option<PathBuf>,
        history: &[Record],
    ) -> anyhow::Result<()> {
        self.open_session(token, cwd, history::rebuild(history));
        Ok(())
    }

    fn send_prompt(
        &self,
        session: &str,
        prompt: claudesktop_core::UserPrompt,
    ) -> anyhow::Result<()> {
        let flag = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let state =
                sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
            if state.turn.is_some() {
                anyhow::bail!("上一轮还没结束");
            }
            // 说明文件可能改过：每轮重读（内容不变时系统提示词不变，不影响提示缓存）
            state.instructions =
                instructions::load(&state.cwd, self.inner.user_instructions.as_deref());
            // 文本 + 图片附件合成一条多段用户消息
            let mut parts = vec![ContentPart::Text(prompt.text)];
            for image in prompt.images {
                parts.push(ContentPart::Image { media_type: image.media_type, data: image.data });
            }
            state.history.push(ChatMessage::with_parts("user", parts));
            let flag = Arc::new(AtomicBool::new(false));
            state.turn = Some(flag.clone());
            flag
        };
        let inner = self.inner.clone();
        let session = session.to_string();
        std::thread::Builder::new()
            .name("builtin-turn".into())
            .spawn(move || run_turn(inner, session, flag))
            .map_err(|e| anyhow::anyhow!("spawn turn thread: {e}"))?;
        Ok(())
    }

    fn cancel(&self, session: &str) -> anyhow::Result<()> {
        // 持锁置位：与 commit() 的判断互斥，取消后这一轮的结果不会再写进历史
        let cancelled = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            match sessions.get_mut(session).and_then(|s| s.turn.take()) {
                Some(flag) => {
                    flag.store(true, Ordering::SeqCst);
                    true
                }
                None => false,
            }
        };
        if cancelled {
            self.inner
                .emit(Some(session), KernelEvent::TurnEnded { stop_reason: "cancelled".into() });
        }
        Ok(())
    }

    fn resolve_permission(&self, request_id: u64, option_id: Option<String>) -> anyhow::Result<()> {
        let tx = self.inner.pending.lock().unwrap().remove(&request_id);
        match tx {
            Some(tx) => {
                let _ = tx.send(option_id);
                Ok(())
            }
            None => anyhow::bail!("权限请求 {request_id} 已失效"),
        }
    }

    fn set_mode(&self, session: &str, mode_id: &str) -> anyhow::Result<()> {
        if !modes().iter().any(|m| m.id == mode_id) {
            anyhow::bail!("未知模式 {mode_id}");
        }
        let mut sessions = self.inner.sessions.lock().unwrap();
        let state =
            sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        state.mode = mode_id.to_string();
        Ok(())
    }

    fn set_model(&self, session: &str, model: &str) -> anyhow::Result<()> {
        let model = model.trim();
        if model.is_empty() {
            anyhow::bail!("模型 ID 为空");
        }
        let mut sessions = self.inner.sessions.lock().unwrap();
        let state =
            sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        // 进行中的一轮用完当前模型，下一步请求起生效
        state.model = model.to_string();
        *self.inner.default_model.lock().unwrap() = model.to_string();
        Ok(())
    }

    fn set_context_windows(&self, windows: HashMap<String, u64>) {
        *self.inner.context_windows.lock().unwrap() = windows;
    }

    /// 手动压缩：自成一轮（占着 `turn`，期间不能发消息、可以取消），全部对话进摘要。
    fn compact(&self, session: &str) -> anyhow::Result<()> {
        let (flag, model) = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let state =
                sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
            if state.turn.is_some() {
                anyhow::bail!("这一轮还没结束");
            }
            if state.history.is_empty() {
                anyhow::bail!("还没有可以压缩的对话");
            }
            let flag = Arc::new(AtomicBool::new(false));
            state.turn = Some(flag.clone());
            (flag, state.model.clone())
        };
        let inner = self.inner.clone();
        let session = session.to_string();
        std::thread::Builder::new()
            .name("builtin-compact".into())
            .spawn(move || {
                let budget = transcript_budget(inner.window(&model));
                let result = inner.compact(&session, &flag, &model, true, 0, budget);
                if flag.load(Ordering::SeqCst) {
                    return; // cancel() 已经报告过 TurnEnded
                }
                inner.finish(&session, &flag);
                if let Ok(Some(u)) = result {
                    inner.emit(
                        Some(&session),
                        KernelEvent::UsageUpdated {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                            reasoning_tokens: u.reasoning_tokens,
                            context_tokens: 0,
                            cache_read_tokens: u.cache_read_tokens,
                        },
                    );
                }
                // 失败已经以 CompactFailed 报过，这一轮照常结束
                inner.emit(
                    Some(&session),
                    KernelEvent::TurnEnded { stop_reason: "end_turn".into() },
                );
            })
            .map_err(|e| anyhow::anyhow!("spawn compact thread: {e}"))?;
        Ok(())
    }

    fn shutdown(&self) {
        for state in self.inner.sessions.lock().unwrap().values_mut() {
            if let Some(flag) = state.turn.take() {
                flag.store(true, Ordering::SeqCst);
            }
        }
    }
}
