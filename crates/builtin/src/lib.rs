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
//!
//! 线程模型：每轮对话一个线程（阻塞读 SSE 流、执行工具、等权限应答）；
//! 取消 = 置该轮的标志位，各阻塞点轮询检查，取消后不再发出任何事件。

mod history;
mod openai;
mod permissions;
mod sse;
mod tools;

pub use openai::{
    ChatMessage, FunctionCall, OpenAiProvider, ProviderConfig, StreamEvent, StreamOutcome,
    ToolCall, Usage,
};
pub use permissions::{PermissionRules, default_rules_path};

/// 列出端点提供的模型（阻塞，设置页在后台线程调用）。
pub fn fetch_models(config: ProviderConfig) -> anyhow::Result<Vec<String>> {
    OpenAiProvider::new(config).list_models()
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
}

impl BuiltinConfig {
    /// 规则文件放在默认数据目录。
    pub fn new(provider: ProviderConfig, cwd: PathBuf) -> Self {
        Self {
            provider,
            cwd,
            rules_path: default_rules_path(),
        }
    }

    /// 从环境变量读取：
    /// - `CLAUDESKTOP_BASE_URL`（必填）：OpenAI 兼容端点，如 `http://127.0.0.1:8045/v1`
    /// - `CLAUDESKTOP_MODEL`（必填）：模型 ID
    /// - `CLAUDESKTOP_API_KEY`（可选，其次 `OPENAI_API_KEY`）
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let base_url = var("CLAUDESKTOP_BASE_URL")
            .ok_or_else(|| anyhow::anyhow!("未设置 CLAUDESKTOP_BASE_URL（OpenAI 兼容端点）"))?;
        let model =
            var("CLAUDESKTOP_MODEL").ok_or_else(|| anyhow::anyhow!("未设置 CLAUDESKTOP_MODEL"))?;
        Ok(Self::new(
            ProviderConfig {
                base_url,
                api_key: var("CLAUDESKTOP_API_KEY").or_else(|| var("OPENAI_API_KEY")),
                model,
            },
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ))
    }

    /// 写进会话元信息的内核标识：同一端点 + 模型才算同一个「agent」。
    pub fn identity(&self) -> String {
        format!("builtin:{}#{}", self.provider.base_url, self.provider.model)
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
    /// 进行中的一轮的取消标志
    turn: Option<Arc<AtomicBool>>,
}

struct Inner {
    provider: OpenAiProvider,
    /// 新会话的默认模型（最近一次 `set_model` 选的，初始为配置里的）
    default_model: Mutex<String>,
    sink: EventSink,
    agent_name: String,
    default_cwd: PathBuf,
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
        (self.sink)(RoutedEvent {
            session: session.map(str::to_owned),
            event,
        });
    }

    /// 本步要发给模型的消息、当前模式、工作目录与模型；会话已不存在返回 None。
    fn snapshot(&self, session: &str) -> Option<(Vec<ChatMessage>, String, PathBuf, String)> {
        let sessions = self.sessions.lock().unwrap();
        let state = sessions.get(session)?;
        let mut messages = vec![ChatMessage::new(
            "system",
            system_prompt(&state.cwd, &state.mode, state.restored),
        )];
        messages.extend(state.history.iter().cloned());
        Some((messages, state.mode.clone(), state.cwd.clone(), state.model.clone()))
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
        if let Some(state) = self.sessions.lock().unwrap().get_mut(session) {
            if state.turn.as_ref().is_some_and(|f| Arc::ptr_eq(f, flag)) {
                state.turn = None;
            }
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
            self.emit(
                Some(session),
                KernelEvent::ToolCallDiff {
                    id: call.id.clone(),
                    diff,
                },
            );
        }
        let output = match action {
            Err(message) => ToolOutput::fail(message, "参数无效"),
            Ok(action) => match self.authorize(session, cwd, &action, &title, stopped) {
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
                },
            );
        }
        output
    }

    /// 读工具直接放行；其余按模式 → 已保存规则 → 询问用户。
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
            MODE_PLAN => {
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
        if let Some(rule) = &rule {
            if self.rules.lock().unwrap().allows(cwd, rule, command) {
                return Verdict::Allow;
            }
        }

        let option = |id: &str, name: String| PermissionOptionView {
            id: id.into(),
            name,
            kind: id.into(),
        };
        let mut options = vec![option("allow_once", "允许一次".into())];
        if let Some(rule) = &rule {
            let name = match rule.strip_prefix("shell:") {
                Some(prefix) => format!("总是允许 `{prefix}` 开头的命令"),
                None => "本项目内总是允许编辑".into(),
            };
            options.push(option("allow_always", name));
        }
        options.push(option("reject_once", "拒绝".into()));

        match self.ask(session, title, options, stopped).as_deref() {
            Some("allow_once") => Verdict::Allow,
            Some("allow_always") => {
                if let Some(rule) = rule {
                    if let Err(e) = self.rules.lock().unwrap().add(cwd, rule) {
                        self.emit(None, KernelEvent::Status(format!("保存权限规则失败: {e}")));
                    }
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
            KernelEvent::PermissionRequest {
                id,
                title: title.to_string(),
                options,
            },
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
            provider: OpenAiProvider::new(config.provider.clone()),
            default_model: Mutex::new(config.provider.model.clone()),
            sink,
            agent_name: agent_name.clone(),
            default_cwd: config.cwd,
            sessions: Mutex::default(),
            next_session: AtomicU64::new(1),
            rules: Mutex::new(PermissionRules::load(config.rules_path)),
            pending: Mutex::default(),
            next_request: AtomicU64::new(1),
        });
        inner.emit(
            None,
            KernelEvent::Status(format!("自研内核: {}", config.provider.base_url)),
        );
        inner.emit(None, KernelEvent::Connected { agent_name });
        Ok(Self { inner })
    }

    /// 新建会话（`history` 非空 = 续接），报告 `Started`。
    fn open_session(&self, token: u64, cwd: Option<PathBuf>, history: Vec<ChatMessage>) {
        let n = self.inner.next_session.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("builtin-{}-{n}", std::process::id());
        let cwd = cwd.unwrap_or_else(|| self.inner.default_cwd.clone());
        let resumed = !history.is_empty();
        let model = self.inner.default_model.lock().unwrap().clone();
        self.inner.sessions.lock().unwrap().insert(
            session_id.clone(),
            SessionState {
                cwd,
                model: model.clone(),
                history,
                mode: MODE_DEFAULT.into(),
                restored: resumed,
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

fn system_prompt(cwd: &Path, mode: &str, restored: bool) -> String {
    let mut prompt = format!(
        "You are a coding agent running inside claudesktop, a desktop agent harness, \
         working on the user's machine.\n\
         Working directory: {}\n\
         Platform: {}\n\n\
         Use the tools to inspect and change the project: find files with glob and search code with grep \
         (rather than shell commands), read files before editing them, \
         prefer edit_file for small changes, and verify your work (e.g. build or run tests) \
         when it makes sense. Tool calls that change things may need the user's approval; \
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

/// 一轮对话（在独立线程里运行）：请求模型 → 执行工具 → 再请求，直到不再调用工具。
fn run_turn(inner: Arc<Inner>, session: String, flag: Arc<AtomicBool>) {
    let stopped = || flag.load(Ordering::SeqCst);
    let mut usage: Option<Usage> = None;
    let mut stop = "max_turn_requests".to_string();

    for _ in 0..MAX_STEPS {
        let Some((messages, mode, cwd, model)) = inner.snapshot(&session) else {
            return;
        };
        let specs = tools::specs(mode != MODE_PLAN);
        let mut text = String::new();
        let result = inner
            .provider
            .stream_chat(&model, &messages, &specs, &stopped, &mut |event| {
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
                    StreamEvent::Retrying {
                        attempt,
                        delay_ms,
                        reason,
                    } => inner.emit(
                        None,
                        KernelEvent::Status(format!("{reason}；{delay_ms}ms 后第 {attempt} 次重试")),
                    ),
                }
            });
        if stopped() {
            return; // cancel() 已经报告过 TurnEnded
        }
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(err) => {
                inner.finish(&session, &flag);
                inner.emit(
                    Some(&session),
                    KernelEvent::PromptFailed {
                        message: format!("{err:#}"),
                    },
                );
                return;
            }
        };
        if let Some(u) = outcome.usage {
            usage.get_or_insert_default().add(u);
        }

        if outcome.tool_calls.is_empty() {
            if !text.is_empty() {
                inner.commit(&session, &flag, vec![ChatMessage::new("assistant", text)]);
            }
            stop = stop_reason(outcome.finish_reason.as_deref());
            break;
        }

        // 同一步的多个调用依次执行；assistant 消息和全部结果一起写进历史，
        // 保证 tool_calls 后面总跟着对应的 tool 消息
        let mut step = Vec::with_capacity(outcome.tool_calls.len() + 1);
        let mut results = Vec::with_capacity(outcome.tool_calls.len());
        for call in &outcome.tool_calls {
            let output = inner.run_tool(&session, &cwd, call, &stopped);
            if stopped() {
                return;
            }
            results.push(ChatMessage::tool_result(call.id.clone(), output.content));
        }
        step.push(ChatMessage::assistant_tool_calls(text, outcome.tool_calls));
        step.extend(results);
        if !inner.commit(&session, &flag, step) {
            return;
        }
    }

    inner.finish(&session, &flag);
    if let Some(u) = usage {
        inner.emit(
            Some(&session),
            KernelEvent::UsageUpdated {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                reasoning_tokens: u.reasoning_tokens,
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
        self.open_session(token, cwd, Vec::new());
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

    fn send_prompt(&self, session: &str, text: String) -> anyhow::Result<()> {
        let flag = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let state = sessions
                .get_mut(session)
                .ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
            if state.turn.is_some() {
                anyhow::bail!("上一轮还没结束");
            }
            state.history.push(ChatMessage::new("user", text));
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
            self.inner.emit(
                Some(session),
                KernelEvent::TurnEnded {
                    stop_reason: "cancelled".into(),
                },
            );
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
        let state = sessions
            .get_mut(session)
            .ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        state.mode = mode_id.to_string();
        Ok(())
    }

    fn set_model(&self, session: &str, model: &str) -> anyhow::Result<()> {
        let model = model.trim();
        if model.is_empty() {
            anyhow::bail!("模型 ID 为空");
        }
        let mut sessions = self.inner.sessions.lock().unwrap();
        let state = sessions
            .get_mut(session)
            .ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        // 进行中的一轮用完当前模型，下一步请求起生效
        state.model = model.to_string();
        *self.inner.default_model.lock().unwrap() = model.to_string();
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
