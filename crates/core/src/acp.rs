//! [`AcpKernel`]：通过 Agent Client Protocol (v1) 驱动任意 ACP agent 子进程。
//!
//! 架构：
//!
//! ```text
//! UI 线程                        内核线程（smol runtime）
//!   │ send_prompt/cancel            │
//!   ├─ futures mpsc ──────────────►│ command loop
//!   │                              │   initialize → (session/new → prompts) × N
//!   │                              │ handlers: session/update, request_permission,
//!  EventSink(回调) ◄──────────────┤           fs/read_text_file, fs/write_text_file
//!   └─（投递回 UI 的通道由宿主决定）  │ AcpAgent 子进程（stdio JSON-RPC）
//! ```
//!
//! 本实现完全复用 `agent-client-protocol` crate 的类型系统与连接层，
//! 不手写 JSON-RPC——协议升级时跟随 crate 版本即可。

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use agent_client_protocol::schema::v1::SessionUpdate;
use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, FileSystemCapabilities, Implementation,
    InitializeRequest, NewSessionRequest, PermissionOptionKind, PromptRequest,
    ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification,
    SetSessionModeRequest, TextContent, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo};
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::channel::oneshot;
use futures::StreamExt;

use crate::{
    EventSink, Kernel, KernelCommand, KernelEvent, ModeView, PermissionOptionView, PlanEntryView,
    RoutedEvent,
};

/// 发出一个事件；`session` 为空表示连接级事件。
fn emit(sink: &EventSink, session: Option<String>, event: KernelEvent) {
    sink(RoutedEvent { session, event });
}

/// 等待 UI 应答的权限请求：请求 ID → 唤醒发送端（`None` = 取消）。
type PendingPermissions = Arc<Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>>;

/// ACP 内核配置。
#[derive(Debug, Clone)]
pub struct AcpKernelConfig {
    /// agent 启动命令（shell-words 语法，或 `{"command":..,"args":[..]}` JSON）。
    ///
    /// 例：`node C:/path/to/mock-acp-agent.mjs`、
    /// `cmd /c npx -y @zed-industries/claude-code-acp`
    pub command: String,
    /// 新会话的默认工作目录（传给 `session/new`，须为绝对路径）
    pub cwd: PathBuf,
    /// 是否打印 wire 级日志（stderr，按行）
    pub debug_wire: bool,
    /// true = 权限请求由内核按策略自动批准（无人值守 / 测试）；
    /// false = 发出 [`KernelEvent::PermissionRequest`] 交给 UI 决定。
    pub auto_approve: bool,
}

impl AcpKernelConfig {
    /// 从环境变量构造（供桌面端使用）：
    /// - `ACP_AGENT`：agent 启动命令；缺省用本地 mock agent
    /// - `ACP_CWD`：会话工作目录；缺省用当前目录
    /// - `ACP_DEBUG_WIRE=1`：打开 wire 日志
    /// - `ACP_AUTO_APPROVE=1`：权限请求自动批准（缺省交给 UI 审批）
    pub fn from_env() -> Self {
        let cwd = std::env::var("ACP_CWD")
            .map(PathBuf::from)
            .ok()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let command = std::env::var("ACP_AGENT").unwrap_or_else(|_| default_agent_command());
        Self {
            command,
            cwd,
            debug_wire: std::env::var("ACP_DEBUG_WIRE").is_ok_and(|v| v == "1"),
            auto_approve: std::env::var("ACP_AUTO_APPROVE").is_ok_and(|v| v == "1"),
        }
    }

    /// 自动批准开关（编程覆盖）。
    pub fn with_auto_approve(mut self, on: bool) -> Self {
        self.auto_approve = on;
        self
    }

    /// wire 日志开关（编程覆盖）。
    pub fn with_debug_wire(mut self, on: bool) -> Self {
        self.debug_wire = on;
        self
    }
}

/// 默认内核：仓库内的 mock ACP agent（零外部依赖，M1 冒烟用）。
fn default_agent_command() -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mock = manifest
        .join("../../tools/mock-acp-agent.mjs")
        .display()
        .to_string()
        .replace('\\', "/");
    format!("node \"{mock}\"")
}

/// ACP 内核句柄（克隆廉价：仅持 channel 发送端）。
///
/// 一个句柄 = 一条到 agent 子进程的连接；连接上可以开任意多个会话，
/// 各会话的事件靠 [`RoutedEvent::session`] 区分。
#[derive(Clone)]
pub struct AcpKernel {
    command_tx: UnboundedSender<KernelCommand>,
    pending: PendingPermissions,
}

impl AcpKernel {
    /// 启动内核：spawn agent 子进程 + 完成握手 + 进入指令循环。
    ///
    /// 返回的句柄立即可用；握手完成时经 sink 收到 `Connected`，
    /// 之后用 [`Kernel::new_session`] 创建会话。
    pub fn spawn(config: AcpKernelConfig, sink: EventSink) -> anyhow::Result<Self> {
        let (tx, rx) = unbounded::<KernelCommand>();
        let pending: PendingPermissions = Arc::default();
        let pending_for_thread = pending.clone();
        emit(
            &sink,
            None,
            KernelEvent::Status(format!("启动 ACP 内核: {}", config.command)),
        );

        let _handle: JoinHandle<()> = std::thread::Builder::new()
            .name("acp-kernel".into())
            .spawn(move || run_kernel(config, rx, sink, pending_for_thread))
            .map_err(|e| anyhow::anyhow!("spawn acp-kernel thread: {e}"))?;

        Ok(Self {
            command_tx: tx,
            pending,
        })
    }

    fn send_command(&self, command: KernelCommand) -> anyhow::Result<()> {
        self.command_tx
            .unbounded_send(command)
            .map_err(|_| anyhow::anyhow!("acp kernel already closed"))
    }
}

impl Kernel for AcpKernel {
    fn kind(&self) -> &'static str {
        "acp"
    }

    fn label(&self) -> String {
        "ACP agent".into()
    }

    fn new_session(&self, token: u64, cwd: Option<PathBuf>) -> anyhow::Result<()> {
        self.send_command(KernelCommand::NewSession { token, cwd })
    }

    fn send_prompt(&self, session: &str, text: String) -> anyhow::Result<()> {
        self.send_command(KernelCommand::Prompt {
            session: session.to_string(),
            text,
        })
    }

    fn cancel(&self, session: &str) -> anyhow::Result<()> {
        self.send_command(KernelCommand::Cancel {
            session: session.to_string(),
        })
    }

    fn resolve_permission(&self, request_id: u64, option_id: Option<String>) -> anyhow::Result<()> {
        let sender = self.pending.lock().unwrap().remove(&request_id);
        match sender {
            Some(tx) => tx
                .send(option_id)
                .map_err(|_| anyhow::anyhow!("permission request {request_id} already closed")),
            None => Err(anyhow::anyhow!("unknown permission request {request_id}")),
        }
    }

    fn set_mode(&self, session: &str, mode_id: &str) -> anyhow::Result<()> {
        self.send_command(KernelCommand::SetMode {
            session: session.to_string(),
            mode_id: mode_id.to_string(),
        })
    }

    fn shutdown(&self) {
        let _ = self.command_tx.unbounded_send(KernelCommand::Shutdown);
    }
}

/// 内核线程主体。
fn run_kernel(
    config: AcpKernelConfig,
    mut cmd_rx: futures::channel::mpsc::UnboundedReceiver<KernelCommand>,
    sink: EventSink,
    pending: PendingPermissions,
) {
    let sink_fatal = sink.clone();
    let result = smol::block_on(async move {
        let mut agent = AcpAgent::from_str(&config.command)
            .map_err(|e| anyhow::anyhow!("parse agent command: {e}"))?;
        if config.debug_wire {
            agent = agent.with_debug(|line, dir| eprintln!("[wire {dir:?}] {line}"));
        }

        // ---- handlers ------------------------------------------------

        let sink_updates = sink.clone();
        let sink_permission = sink.clone();
        let sink_fs_read = sink.clone();
        let sink_fs_write = sink.clone();

        let default_cwd = config.cwd.clone();
        let auto_approve = config.auto_approve;
        let next_request_id = Arc::new(AtomicU64::new(1));

        Client
            .builder()
            .on_receive_notification(
                async move |notification: SessionNotification, _connection| {
                    map_notification(&sink_updates, &notification);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: RequestPermissionRequest, responder, connection| {
                    let session = request.session_id.to_string();
                    let title = request
                        .tool_call
                        .fields
                        .title
                        .clone()
                        .unwrap_or_else(|| request.tool_call.tool_call_id.to_string());

                    if !auto_approve {
                        // 交互审批：事件交给 UI，应答挂在独立任务里等待——
                        // 不能阻塞协议库的事件循环（否则 session/update 也会卡住）。
                        let id = next_request_id.fetch_add(1, Ordering::Relaxed);
                        let (tx, rx) = oneshot::channel::<Option<String>>();
                        pending.lock().unwrap().insert(id, tx);
                        let options = request
                            .options
                            .iter()
                            .map(|o| PermissionOptionView {
                                id: o.option_id.to_string(),
                                name: o.name.clone(),
                                kind: wire_name(&o.kind),
                            })
                            .collect();
                        emit(
                            &sink_permission,
                            Some(session),
                            KernelEvent::PermissionRequest { id, title, options },
                        );
                        return connection.spawn(async move {
                            let outcome = match rx.await.ok().flatten() {
                                Some(option_id) => RequestPermissionOutcome::Selected(
                                    SelectedPermissionOutcome::new(option_id),
                                ),
                                None => RequestPermissionOutcome::Cancelled,
                            };
                            responder.respond(RequestPermissionResponse::new(outcome))
                        });
                    }

                    // 自动批准策略：allow_once > allow_always > 首个选项
                    let chosen = request
                        .options
                        .iter()
                        .find(|o| matches!(o.kind, PermissionOptionKind::AllowOnce))
                        .or_else(|| {
                            request
                                .options
                                .iter()
                                .find(|o| matches!(o.kind, PermissionOptionKind::AllowAlways))
                        })
                        .or_else(|| request.options.first());
                    match chosen {
                        Some(option) => {
                            emit(
                                &sink_permission,
                                Some(session),
                                KernelEvent::PermissionAuto {
                                    chosen: option.name.clone(),
                                    title,
                                },
                            );
                            responder.respond(RequestPermissionResponse::new(
                                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                    option.option_id.clone(),
                                )),
                            ))
                        }
                        None => {
                            emit(
                                &sink_permission,
                                Some(session),
                                KernelEvent::PermissionAuto {
                                    chosen: "(已取消：无选项)".into(),
                                    title,
                                },
                            );
                            responder.respond(RequestPermissionResponse::new(
                                RequestPermissionOutcome::Cancelled,
                            ))
                        }
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: ReadTextFileRequest, responder, _connection| {
                    let session = request.session_id.to_string();
                    let path = request.path.to_string_lossy().into_owned();
                    // M1：忽略 line/limit；失败以空内容应答并上报事件
                    let content = match std::fs::read_to_string(&request.path) {
                        Ok(content) => {
                            emit(
                                &sink_fs_read,
                                Some(session),
                                KernelEvent::FsRead { path, ok: true },
                            );
                            content
                        }
                        Err(err) => {
                            emit(
                                &sink_fs_read,
                                Some(session),
                                KernelEvent::FsRead {
                                    path: format!("{path} ({err})"),
                                    ok: false,
                                },
                            );
                            String::new()
                        }
                    };
                    responder.respond(ReadTextFileResponse::new(content))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: WriteTextFileRequest, responder, _connection| {
                    let session = request.session_id.to_string();
                    let path = request.path.to_string_lossy().into_owned();
                    let ok = std::fs::write(&request.path, &request.content).is_ok();
                    emit(
                        &sink_fs_write,
                        Some(session),
                        KernelEvent::FsWrite { path, ok },
                    );
                    responder.respond(WriteTextFileResponse::new())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(agent, move |connection: ConnectionTo<Agent>| async move {
                // ---- 握手 ------------------------------------------------
                let init = connection
                    .send_request(
                        InitializeRequest::new(ProtocolVersion::V1)
                            .client_capabilities(
                                ClientCapabilities::default().fs(
                                    FileSystemCapabilities::new()
                                        .read_text_file(true)
                                        .write_text_file(true),
                                ),
                            )
                            .client_info(Implementation::new(
                                "claudesktop",
                                env!("CARGO_PKG_VERSION"),
                            )),
                    )
                    .block_task()
                    .await?;
                let agent_name = init
                    .agent_info
                    .map(|info| info.name)
                    .unwrap_or_else(|| "unknown-agent".into());
                emit(
                    &sink,
                    None,
                    KernelEvent::Status(format!(
                        "协议握手成功: v{} / {agent_name}",
                        init.protocol_version.as_u16()
                    )),
                );
                emit(
                    &sink,
                    None,
                    KernelEvent::Connected {
                        agent_name: agent_name.clone(),
                    },
                );

                // ---- 指令循环 --------------------------------------------
                while let Some(command) = cmd_rx.next().await {
                    match command {
                        KernelCommand::NewSession { token, cwd } => {
                            let cwd = cwd.unwrap_or_else(|| default_cwd.clone());
                            let created = connection
                                .send_request(NewSessionRequest::new(cwd))
                                .block_task()
                                .await;
                            match created {
                                Ok(session) => {
                                    let model = current_model(&session.config_options);
                                    let (modes, current_mode) = match &session.modes {
                                        Some(state) => (
                                            state
                                                .available_modes
                                                .iter()
                                                .map(|m| ModeView {
                                                    id: m.id.to_string(),
                                                    name: m.name.clone(),
                                                    description: m.description.clone(),
                                                })
                                                .collect(),
                                            Some(state.current_mode_id.to_string()),
                                        ),
                                        None => (Vec::new(), None),
                                    };
                                    let session_id = session.session_id.to_string();
                                    emit(
                                        &sink,
                                        Some(session_id.clone()),
                                        KernelEvent::Started {
                                            token,
                                            agent_name: agent_name.clone(),
                                            session_id,
                                            model,
                                            modes,
                                            current_mode,
                                        },
                                    );
                                }
                                Err(err) => emit(
                                    &sink,
                                    None,
                                    KernelEvent::SessionFailed {
                                        token,
                                        message: format!("{err}"),
                                    },
                                ),
                            }
                        }
                        KernelCommand::Prompt { session, text } => {
                            // 在独立任务里等本轮结束：指令循环必须保持空闲，
                            // 否则一轮进行中的 Cancel / SetMode / 其他会话的提示
                            // 都要等到轮末才处理。
                            let sink_turn = sink.clone();
                            let conn = connection.clone();
                            connection.spawn(async move {
                                let response = conn
                                    .send_request(PromptRequest::new(
                                        session.clone(),
                                        vec![ContentBlock::Text(TextContent::new(text))],
                                    ))
                                    .block_task()
                                    .await;
                                let event = match response {
                                    Ok(r) => KernelEvent::TurnEnded {
                                        stop_reason: wire_name(&r.stop_reason),
                                    },
                                    Err(err) => KernelEvent::PromptFailed {
                                        message: format!("{err}"),
                                    },
                                };
                                emit(&sink_turn, Some(session), event);
                                Ok(())
                            })?;
                        }
                        KernelCommand::Cancel { session } => {
                            let _ = connection.send_notification(CancelNotification::new(session));
                        }
                        KernelCommand::SetMode { session, mode_id } => {
                            let result = connection
                                .send_request(SetSessionModeRequest::new(
                                    session.clone(),
                                    mode_id.clone(),
                                ))
                                .block_task()
                                .await;
                            emit(
                                &sink,
                                Some(session),
                                KernelEvent::Status(match result {
                                    Ok(_) => format!("权限模式已切换: {mode_id}"),
                                    Err(err) => format!("切换权限模式失败（{mode_id}）: {err}"),
                                }),
                            );
                        }
                        KernelCommand::Shutdown => break,
                    }
                }
                emit(&sink, None, KernelEvent::Status("内核连接关闭".into()));
                Ok(())
            })
            .await
    });

    if let Err(err) = result {
        emit(&sink_fatal, None, KernelEvent::Fatal(format!("{err:#}")));
    }
}

/// 把 ACP `session/update` 通知映射为 [`KernelEvent`]。
fn map_notification(sink: &EventSink, notification: &SessionNotification) {
    let event = match &notification.update {
        SessionUpdate::UserMessageChunk(chunk) => text_of(&chunk.content)
            .map(|text| KernelEvent::UserMessageChunk { text }),
        SessionUpdate::AgentMessageChunk(chunk) => text_of(&chunk.content)
            .map(|text| KernelEvent::AgentMessageChunk { text }),
        SessionUpdate::AgentThoughtChunk(chunk) => text_of(&chunk.content)
            .map(|text| KernelEvent::AgentThoughtChunk { text }),
        SessionUpdate::ToolCall(call) => Some(KernelEvent::ToolCallStarted {
            id: call.tool_call_id.to_string(),
            title: call.title.clone(),
            status: wire_name(&call.status),
            kind: wire_name(&call.kind),
        }),
        SessionUpdate::ToolCallUpdate(update) => Some(KernelEvent::ToolCallUpdated {
            id: update.tool_call_id.to_string(),
            title: update.fields.title.clone(),
            status: update.fields.status.as_ref().map(wire_name),
            raw_output: update
                .fields
                .raw_output
                .as_ref()
                .map(|v| truncate(&output_text(v), 2000)),
        }),
        SessionUpdate::Plan(plan) => Some(KernelEvent::PlanReplaced {
            entries: plan
                .entries
                .iter()
                .map(|entry| PlanEntryView {
                    content: entry.content.clone(),
                    priority: wire_name(&entry.priority),
                    status: wire_name(&entry.status),
                })
                .collect(),
        }),
        // 其余变体（usage/notice/mode/commands…）M1 统一降级为 Status
        other => Some(KernelEvent::Status(format!("update: {other:?}"))),
    };
    if let Some(event) = event {
        emit(sink, Some(notification.session_id.to_string()), event);
    }
}

/// 从 `session/new` 的 `config_options` 里取当前模型（协议类型走 serde 取值，
/// 避免绑死 Select/Boolean 等变体的具体结构）。
fn current_model(options: &impl serde::Serialize) -> Option<String> {
    let value = serde_json::to_value(options).ok()?;
    value.as_array()?.iter().find_map(|opt| {
        let is_model = opt.get("category").and_then(|v| v.as_str()) == Some("model")
            || opt.get("id").and_then(|v| v.as_str()) == Some("model");
        if !is_model {
            return None;
        }
        opt.get("currentValue")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    })
}

/// 工具输出 → 给人看的文本。agent 的输出常是 `{"content":[{"type":"text",
/// "text":"…"}]}` 这类结构，直接取文本；取不到再退回 JSON 文本。
fn output_text(value: &serde_json::Value) -> String {
    fn extract(v: &serde_json::Value) -> Option<String> {
        match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Array(items) => {
                let parts: Vec<String> = items.iter().filter_map(extract).collect();
                (!parts.is_empty()).then(|| parts.join("\n"))
            }
            serde_json::Value::Object(map) => ["text", "content", "stdout", "output", "result"]
                .iter()
                .find_map(|key| map.get(*key).and_then(extract)),
            _ => None,
        }
    }
    extract(value).unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn output_text_extracts_nested_text() {
        let v = json!({"content": [{"type": "text", "text": "hello"}, {"type": "text", "text": "world"}]});
        assert_eq!(output_text(&v), "hello\nworld");
        assert_eq!(output_text(&json!("plain")), "plain");
        assert_eq!(output_text(&json!({"stdout": "ok", "code": 0})), "ok");
    }

    #[test]
    fn output_text_falls_back_to_json() {
        let v = json!({"granted": true, "lines": 148});
        let out = output_text(&v);
        assert!(out.contains("granted") && out.contains("148"), "{out}");
    }
}

/// 取文本内容块；M1 只处理 Text，其余（图片/资源）忽略。
fn text_of(content: &ContentBlock) -> Option<String> {
    match content {
        ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    }
}

/// 枚举的 wire 名（serde 序列化形式，如 `in_progress`、`end_turn`）。
///
/// 比 `format!("{:?}")` 稳：Debug 输出与协议字符串可能不一致
/// （`InProgress` vs `in_progress`），而 UI/测试都以 wire 名对齐。
fn wire_name<T: serde::Serialize + std::fmt::Debug>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{value:?}"))
}

/// 截断长字符串（尾部加省略号）。
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}
