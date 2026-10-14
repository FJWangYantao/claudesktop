//! # claudesktop-builtin
//!
//! 自研内核（进程内）：直接调 LLM provider，产出与 ACP 内核同一套
//! [`KernelEvent`]，UI 无感切换。
//!
//! M2 第一步：OpenAI 兼容 provider + 纯文本 / 思考流式对话 + 用量；
//! 工具调用循环在下一步加入。
//!
//! 线程模型：每轮对话一个线程（阻塞读 SSE 流）；取消 = 置该轮的标志位，
//! 读流循环逐行检查，取消后不再发出任何事件。

mod openai;
mod sse;

pub use openai::{ChatMessage, OpenAiProvider, ProviderConfig, StreamEvent, StreamOutcome, Usage};

use claudesktop_core::{EventSink, Kernel, KernelEvent, RoutedEvent};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct BuiltinConfig {
    pub provider: ProviderConfig,
    /// 默认工作目录（写进系统提示词）
    pub cwd: PathBuf,
}

impl BuiltinConfig {
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
        Ok(Self {
            provider: ProviderConfig {
                base_url,
                api_key: var("CLAUDESKTOP_API_KEY").or_else(|| var("OPENAI_API_KEY")),
                model,
            },
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        })
    }

    /// 写进会话元信息的内核标识：同一端点 + 模型才算同一个「agent」。
    pub fn identity(&self) -> String {
        format!("builtin:{}#{}", self.provider.base_url, self.provider.model)
    }
}

struct SessionState {
    cwd: PathBuf,
    /// 不含系统提示词的对话历史
    history: Vec<ChatMessage>,
    /// 进行中的一轮的取消标志
    turn: Option<Arc<AtomicBool>>,
}

struct Inner {
    provider: OpenAiProvider,
    sink: EventSink,
    agent_name: String,
    default_cwd: PathBuf,
    sessions: Mutex<HashMap<String, SessionState>>,
    next_session: AtomicU64,
}

impl Inner {
    fn emit(&self, session: Option<&str>, event: KernelEvent) {
        (self.sink)(RoutedEvent {
            session: session.map(str::to_owned),
            event,
        });
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
            sink,
            agent_name: agent_name.clone(),
            default_cwd: config.cwd,
            sessions: Mutex::default(),
            next_session: AtomicU64::new(1),
        });
        inner.emit(
            None,
            KernelEvent::Status(format!("自研内核: {}", config.provider.base_url)),
        );
        inner.emit(None, KernelEvent::Connected { agent_name });
        Ok(Self { inner })
    }
}

fn system_prompt(cwd: &std::path::Path) -> String {
    format!(
        "You are a helpful coding assistant running inside claudesktop, a desktop agent harness.\n\
         Current working directory: {}\n\
         Answer in the user's language. Use Markdown for structure and code.",
        cwd.display()
    )
}

/// provider 的 finish_reason → ACP 风格的 stop_reason。
fn stop_reason(finish: Option<&str>) -> String {
    match finish {
        None | Some("stop") => "end_turn".into(),
        Some("length") => "max_tokens".into(),
        Some(other) => other.into(),
    }
}

/// 一轮对话（在独立线程里运行）。
fn run_turn(inner: Arc<Inner>, session: String, messages: Vec<ChatMessage>, flag: Arc<AtomicBool>) {
    let stopped = || flag.load(Ordering::SeqCst);
    let mut text = String::new();
    let result = inner.provider.stream_chat(&messages, &stopped, &mut |event| {
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

    {
        let mut sessions = inner.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(&session) {
            // 只清自己这一轮（取消后可能已经开始了新的一轮）
            if state.turn.as_ref().is_some_and(|f| Arc::ptr_eq(f, &flag)) {
                state.turn = None;
            }
            // 取消的轮次不写回历史：新一轮可能已经追加了用户消息，避免顺序错乱
            if !stopped() && !text.is_empty() {
                state.history.push(ChatMessage::new("assistant", text));
            }
        }
    }
    if stopped() {
        return; // cancel() 已经报告过 TurnEnded
    }
    match result {
        Ok(outcome) => {
            if let Some(u) = outcome.usage {
                inner.emit(
                    Some(&session),
                    KernelEvent::UsageUpdated {
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                        reasoning_tokens: u.reasoning_tokens,
                    },
                );
            }
            inner.emit(
                Some(&session),
                KernelEvent::TurnEnded {
                    stop_reason: stop_reason(outcome.finish_reason.as_deref()),
                },
            );
        }
        Err(err) => inner.emit(
            Some(&session),
            KernelEvent::PromptFailed {
                message: format!("{err:#}"),
            },
        ),
    }
}

impl Kernel for BuiltinKernel {
    fn kind(&self) -> &'static str {
        "builtin"
    }

    fn label(&self) -> String {
        self.inner.agent_name.clone()
    }

    fn new_session(&self, token: u64, cwd: Option<PathBuf>) -> anyhow::Result<()> {
        let n = self.inner.next_session.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("builtin-{}-{n}", std::process::id());
        let cwd = cwd.unwrap_or_else(|| self.inner.default_cwd.clone());
        self.inner.sessions.lock().unwrap().insert(
            session_id.clone(),
            SessionState {
                cwd,
                history: Vec::new(),
                turn: None,
            },
        );
        self.inner.emit(
            Some(&session_id),
            KernelEvent::Started {
                token,
                agent_name: self.inner.agent_name.clone(),
                session_id: session_id.clone(),
                model: Some(self.inner.provider.model().to_string()),
                modes: Vec::new(),
                current_mode: None,
                resumed: false,
            },
        );
        Ok(())
    }

    fn send_prompt(&self, session: &str, text: String) -> anyhow::Result<()> {
        let (messages, flag) = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let state = sessions
                .get_mut(session)
                .ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
            if state.turn.is_some() {
                anyhow::bail!("上一轮还没结束");
            }
            state.history.push(ChatMessage::new("user", text));
            let mut messages = vec![ChatMessage::new("system", system_prompt(&state.cwd))];
            messages.extend(state.history.iter().cloned());
            let flag = Arc::new(AtomicBool::new(false));
            state.turn = Some(flag.clone());
            (messages, flag)
        };
        let inner = self.inner.clone();
        let session = session.to_string();
        std::thread::Builder::new()
            .name("builtin-turn".into())
            .spawn(move || run_turn(inner, session, messages, flag))
            .map_err(|e| anyhow::anyhow!("spawn turn thread: {e}"))?;
        Ok(())
    }

    fn cancel(&self, session: &str) -> anyhow::Result<()> {
        let flag = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .get_mut(session)
            .and_then(|s| s.turn.take());
        if let Some(flag) = flag {
            flag.store(true, Ordering::SeqCst);
            self.inner.emit(
                Some(session),
                KernelEvent::TurnEnded {
                    stop_reason: "cancelled".into(),
                },
            );
        }
        Ok(())
    }

    fn resolve_permission(&self, request_id: u64, _option_id: Option<String>) -> anyhow::Result<()> {
        anyhow::bail!("自研内核暂无权限请求（{request_id}）")
    }

    fn set_mode(&self, _session: &str, _mode_id: &str) -> anyhow::Result<()> {
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
