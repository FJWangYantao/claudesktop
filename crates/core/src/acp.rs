//! [`AcpKernel`]：通过 Agent Client Protocol (v1) 驱动任意 ACP agent 子进程。
//!
//! 架构：
//!
//! ```text
//! UI 线程                        内核线程（smol runtime）
//!   │ send_prompt/cancel            │
//!   ├─ futures mpsc ──────────────►│ command loop
//!   │                              │   initialize → session/new → prompts
//!   │                              │ handlers: session/update, request_permission,
//!  EventSink(回调) ◄──────────────┤           fs/read_text_file, fs/write_text_file
//!   └─（投递回 UI 的通道由宿主决定）  │ AcpAgent 子进程（stdio JSON-RPC）
//! ```
//!
//! 本实现完全复用 `agent-client-protocol` crate 的类型系统与连接层，
//! 不手写 JSON-RPC——协议升级时跟随 crate 版本即可。

use std::path::PathBuf;
use std::str::FromStr;
use std::thread::JoinHandle;

use agent_client_protocol::schema::v1::SessionUpdate;
use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, FileSystemCapabilities, Implementation,
    InitializeRequest, NewSessionRequest, PermissionOptionKind, PromptRequest,
    ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, TextContent,
    WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo};
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::StreamExt;

use crate::{EventSink, Kernel, KernelCommand, KernelEvent, PlanEntryView};

/// ACP 内核配置。
#[derive(Debug, Clone)]
pub struct AcpKernelConfig {
    /// agent 启动命令（shell-words 语法，或 `{"command":..,"args":[..]}` JSON）。
    ///
    /// 例：`node C:/path/to/mock-acp-agent.mjs`、
    /// `cmd /c npx -y @zed-industries/claude-code-acp`
    pub command: String,
    /// 会话工作目录（传给 `session/new`，须为绝对路径）
    pub cwd: PathBuf,
    /// 是否打印 wire 级日志（stderr，按行）
    pub debug_wire: bool,
}

impl AcpKernelConfig {
    /// 从环境变量构造（供桌面端使用）：
    /// - `ACP_AGENT`：agent 启动命令；缺省用本地 mock agent
    /// - `ACP_CWD`：会话工作目录；缺省用当前目录
    /// - `ACP_DEBUG_WIRE=1`：打开 wire 日志
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
        }
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
#[derive(Clone)]
pub struct AcpKernel {
    command_tx: UnboundedSender<KernelCommand>,
}

impl AcpKernel {
    /// 启动内核：spawn agent 子进程 + 完成握手 + 进入指令循环。
    ///
    /// 返回的句柄立即可用；`Started` 事件经 sink 异步到达
    /// （握手完成时）。
    pub fn spawn(config: AcpKernelConfig, sink: EventSink) -> anyhow::Result<Self> {
        let (tx, rx) = unbounded::<KernelCommand>();
        let sink_status = sink.clone();
        sink_status(KernelEvent::Status(format!(
            "启动 ACP 内核: {}",
            config.command
        )));

        let _handle: JoinHandle<()> = std::thread::Builder::new()
            .name("acp-kernel".into())
            .spawn(move || run_kernel(config, rx, sink))
            .map_err(|e| anyhow::anyhow!("spawn acp-kernel thread: {e}"))?;

        Ok(Self { command_tx: tx })
    }
}

impl Kernel for AcpKernel {
    fn kind(&self) -> &'static str {
        "acp"
    }

    fn label(&self) -> String {
        "ACP agent".into()
    }

    fn send_prompt(&self, text: String) -> anyhow::Result<()> {
        self.command_tx
            .unbounded_send(KernelCommand::Prompt(text))
            .map_err(|_| anyhow::anyhow!("acp kernel already closed"))
    }

    fn cancel(&self) -> anyhow::Result<()> {
        self.command_tx
            .unbounded_send(KernelCommand::Cancel)
            .map_err(|_| anyhow::anyhow!("acp kernel already closed"))
    }

    fn shutdown(&self) {
        let _ = self.command_tx.unbounded_send(KernelCommand::Shutdown);
    }
}

/// 内核线程主体。
fn run_kernel(config: AcpKernelConfig, mut cmd_rx: futures::channel::mpsc::UnboundedReceiver<KernelCommand>, sink: EventSink) {
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

        let cwd = config.cwd.clone();

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
                async move |request: RequestPermissionRequest, responder, _connection| {
                    let title = request
                        .tool_call
                        .fields
                        .title
                        .clone()
                        .unwrap_or_else(|| request.tool_call.tool_call_id.to_string());
                    // M1 策略：allow_once > allow_always > 首个选项
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
                            sink_permission(KernelEvent::PermissionAuto {
                                chosen: option.name.clone(),
                                title,
                            });
                            responder.respond(RequestPermissionResponse::new(
                                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                    option.option_id.clone(),
                                )),
                            ))
                        }
                        None => {
                            sink_permission(KernelEvent::PermissionAuto {
                                chosen: "(已取消：无选项)".into(),
                                title,
                            });
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
                    let path = request.path.to_string_lossy().into_owned();
                    // M1：忽略 line/limit；失败以空内容应答并上报事件
                    let content = match std::fs::read_to_string(&request.path) {
                        Ok(content) => {
                            sink_fs_read(KernelEvent::FsRead { path, ok: true });
                            content
                        }
                        Err(err) => {
                            sink_fs_read(KernelEvent::FsRead {
                                path: format!("{path} ({err})"),
                                ok: false,
                            });
                            String::new()
                        }
                    };
                    responder.respond(ReadTextFileResponse::new(content))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: WriteTextFileRequest, responder, _connection| {
                    let path = request.path.to_string_lossy().into_owned();
                    let ok = std::fs::write(&request.path, &request.content).is_ok();
                    sink_fs_write(KernelEvent::FsWrite { path, ok });
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
                sink(KernelEvent::Status(format!(
                    "协议握手成功: v{} / {agent_name}",
                    init.protocol_version.as_u16()
                )));

                let session = connection
                    .send_request(NewSessionRequest::new(cwd))
                    .block_task()
                    .await?;
                let session_id = session.session_id;
                sink(KernelEvent::Started {
                    agent_name,
                    session_id: session_id.to_string(),
                });

                // ---- 指令循环 --------------------------------------------
                while let Some(command) = cmd_rx.next().await {
                    match command {
                        KernelCommand::Prompt(text) => {
                            let response = connection
                                .send_request(PromptRequest::new(
                                    session_id.clone(),
                                    vec![ContentBlock::Text(TextContent::new(text))],
                                ))
                                .block_task()
                                .await;
                            match response {
                                Ok(r) => {
                                    sink(KernelEvent::TurnEnded {
                                        stop_reason: wire_name(&r.stop_reason),
                                    });
                                }
                                Err(err) => {
                                    sink(KernelEvent::Fatal(format!("prompt failed: {err}")));
                                }
                            }
                        }
                        KernelCommand::Cancel => {
                            let _ = connection
                                .send_notification(CancelNotification::new(session_id.clone()));
                        }
                        KernelCommand::Shutdown => break,
                    }
                }
                sink(KernelEvent::Status("内核连接关闭".into()));
                Ok(())
            })
            .await
    });

    if let Err(err) = result {
        sink_fatal(KernelEvent::Fatal(format!("{err:#}")));
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
        }),
        SessionUpdate::ToolCallUpdate(update) => Some(KernelEvent::ToolCallUpdated {
            id: update.tool_call_id.to_string(),
            title: update.fields.title.clone(),
            status: update.fields.status.as_ref().map(wire_name),
            raw_output: update
                .fields
                .raw_output
                .as_ref()
                .map(|v| truncate(&v.to_string(), 2000)),
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
        sink(event);
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
