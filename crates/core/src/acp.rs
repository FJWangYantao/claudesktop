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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use agent_client_protocol::schema::v1::SessionUpdate;
use agent_client_protocol::schema::v1::{
    AuthMethod, AuthenticateRequest, CancelNotification, ClientCapabilities,
    ClientSessionCapabilities, CompactionCapabilities, ContentBlock, CreateElicitationRequest,
    CreateElicitationResponse, ElicitationAction, ElicitationCapabilities, ElicitationContentValue,
    ElicitationFormCapabilities, ElicitationMode, ElicitationPropertySchema, ElicitationScope,
    EmbeddedResourceResource, EnvVariable, FileSystemCapabilities, HttpHeader, ImageContent,
    Implementation, InitializeRequest, LoadSessionRequest, McpServer, McpServerHttp, McpServerSse,
    McpServerStdio, MultiSelectItems, NewSessionRequest, NoticeCapabilities, PermissionOptionKind,
    PromptRequest, ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, ResumeSessionRequest,
    SelectedPermissionOutcome, SessionConfigKind, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SessionModeState, SessionNotification,
    SetSessionConfigOptionRequest, SetSessionModeRequest, TextContent, ToolCallContent,
    WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::schema::{MaybeUndefined, ProtocolVersion};
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::channel::oneshot;

use crate::{
    AuthMethodKind, AuthMethodView, AvailableCommandView, CompactionView, ConfigChoiceView,
    ConfigOptionView, ContextUsageView, CostView, DiffView, EventSink, Kernel, KernelCommand,
    KernelEvent, ModeView, PermissionOptionView, PlanEntryView, RoutedEvent, SessionKind,
};

/// 发出一个事件；`session` 为空表示连接级事件。
fn emit(sink: &EventSink, session: Option<String>, event: KernelEvent) {
    sink(RoutedEvent { session, event });
}

/// 等待 UI 应答的权限请求：请求 ID → 唤醒发送端（`None` = 取消）。
type PendingPermissions = Arc<Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>>;
/// 等待 UI 应答的 elicitation：请求 ID →（会话，动作发送端）。
type PendingElicitations =
    Arc<Mutex<HashMap<u64, (Option<String>, oneshot::Sender<ElicitationAction>)>>>;

/// 启用的插件带来的 MCP 服务：新开 / 续接会话时交给 agent（`set_plugins` 更新，之后开的会话生效）。
type PluginServers = Arc<Mutex<Vec<McpServer>>>;

#[derive(Default)]
struct AcpShared {
    auth_methods: Mutex<Vec<AuthMethodView>>,
    configs: Mutex<HashMap<String, Vec<ConfigOptionView>>>,
    compactions: Mutex<HashMap<(String, String), CompactionView>>,
    started: Mutex<HashSet<String>>,
    early: Mutex<Vec<SessionNotification>>,
    /// 已经发过至少一条 `session/prompt` 的会话。首轮 Prompt 之前的
    /// `AgentMessageChunk` 不是回复（pi-acp 的开机清单），降级成提示。
    prompted: Mutex<HashSet<String>>,
    /// 收到过正文块的会话（判断「第一条」用）。
    saw_chunk: Mutex<HashSet<String>>,
    /// 这个 agent 的名字（initialize 报的），用来认 pi-acp 的开机清单。
    agent_name: Mutex<String>,
    /// `modes` 其实是思考强度的会话 → 由 `modes` 合成的 `thought_level` 配置项。
    /// agent 刷新 `config_options` 时会把它整表覆盖掉，靠这里合并回去。
    mode_thought: Mutex<HashMap<String, ConfigOptionView>>,
    /// `modes` 报的是思考强度的会话（权限模式交给 UI 的是空表）。
    thinking_mode_sessions: Mutex<HashSet<String>>,
}

/// 由 `modes` 合成的思考配置项的 ID（真 agent 的 ID 不会长这样）。
const MODE_THOUGHT_ID: &str = "__mode_thought_level__";

/// 思考强度的常见档位 ID（忽略大小写）。判「这组 `modes` 像不像思考档位」时，
/// 至少要命中这里的一个，`off` 不算（它同时是通用开关词，见 [`THOUGHT_NEUTRAL_IDS`]）。
const THOUGHT_LEVEL_IDS: &[&str] =
    &["minimal", "min", "low", "medium", "med", "high", "xhigh", "extra_high", "max"];

/// 只在思考档位里出现、权限模式里绝不会有的中性词：`default` / `auto` / `off` 两边都说得通，单列。
const THOUGHT_NEUTRAL_IDS: &[&str] =
    &["off", "none", "on", "enabled", "disabled", "adaptive", "default", "auto"];

/// 这组 `modes` 是不是其实是思考强度（典型：pi-acp 报 `off / minimal / low / medium / high / xhigh`）。
///
/// 判据：全部条目都落在思考词表内，且至少有一个明确属于强度的档位。
/// 权限模式（Claude 的 `default / acceptEdits / plan`、OpenCode 的 `build / plan`）
/// 里 `plan` / `build` 不在词表内，所以不会命中。
fn is_thinking_modes(modes: &[ModeView]) -> bool {
    if modes.is_empty() {
        return false;
    }
    let known = |id: &str, table: &[&str]| table.iter().any(|word| word.eq_ignore_ascii_case(id));
    modes
        .iter()
        .all(|mode| known(&mode.id, THOUGHT_LEVEL_IDS) || known(&mode.id, THOUGHT_NEUTRAL_IDS))
        && modes.iter().any(|mode| known(&mode.id, THOUGHT_LEVEL_IDS))
}

/// 把思考档位表做成一个 `thought_level` 配置项（进模型菜单的 Effort 底栏）。
fn thought_option_from_modes(modes: &[ModeView], current: Option<&str>) -> ConfigOptionView {
    let fallback = modes.first().map(|mode| mode.id.clone()).unwrap_or_default();
    ConfigOptionView {
        id: MODE_THOUGHT_ID.into(),
        name: "思考强度".into(),
        description: None,
        category: "thought_level".into(),
        current_value: current.map(str::to_owned).unwrap_or(fallback),
        options: modes
            .iter()
            .map(|mode| ConfigChoiceView {
                id: mode.id.clone(),
                name: mode.name.clone(),
                description: mode.description.clone(),
                group: None,
            })
            .collect(),
    }
}

/// 会话当下的配置表。`.await` 之后一律重新取，不要拿请求前的快照回写。
fn session_configs(shared: &AcpShared, session: &str) -> Vec<ConfigOptionView> {
    shared.configs.lock().unwrap().get(session).cloned().unwrap_or_default()
}

/// agent 整表刷新 `config_options` 后，把合成的思考项合并回去。
///
/// 原生 `thought_level` 一旦出现就由它接管（合成项作废），避免两条路径各显示一个底栏。
fn merge_mode_thought(shared: &AcpShared, session: &str, options: &mut Vec<ConfigOptionView>) {
    let native = options
        .iter()
        .any(|option| option.id != MODE_THOUGHT_ID && crate::is_selectable_thought(option));
    if native {
        // 原生接管：清掉自己合成的那一份（表和 map 都不留）
        options.retain(|option| option.id != MODE_THOUGHT_ID);
        shared.mode_thought.lock().unwrap().remove(session);
        return;
    }
    let synthesized = shared.mode_thought.lock().unwrap().get(session).cloned();
    if let Some(thought) = synthesized {
        options.retain(|option| option.id != MODE_THOUGHT_ID);
        options.push(thought);
    }
}

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
    /// 额外给 agent 子进程的环境变量（如经用量网关时的 `ANTHROPIC_BASE_URL`）
    pub env: Vec<(String, String)>,
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
            env: Vec::new(),
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
    let mock =
        manifest.join("../../tools/mock-acp-agent.mjs").display().to_string().replace('\\', "/");
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
    elicitation: PendingElicitations,
    plugin_servers: PluginServers,
    resumable: Arc<AtomicBool>,
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
        let elicitation: PendingElicitations = Arc::default();
        let elicitation_for_thread = elicitation.clone();
        let plugin_servers: PluginServers = Arc::default();
        let servers_for_thread = plugin_servers.clone();
        let resumable = Arc::new(AtomicBool::new(false));
        let resumable_for_thread = resumable.clone();
        emit(&sink, None, KernelEvent::Status(format!("启动 ACP 内核: {}", config.command)));

        let _handle: JoinHandle<()> = std::thread::Builder::new()
            .name("acp-kernel".into())
            .spawn(move || {
                run_kernel(
                    config,
                    rx,
                    sink,
                    pending_for_thread,
                    servers_for_thread,
                    resumable_for_thread,
                    elicitation_for_thread,
                )
            })
            .map_err(|e| anyhow::anyhow!("spawn acp-kernel thread: {e}"))?;

        Ok(Self { command_tx: tx, pending, elicitation, plugin_servers, resumable })
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

    fn new_session(&self, token: u64, spec: crate::SessionSpec) -> anyhow::Result<()> {
        self.send_command(KernelCommand::NewSession { token, cwd: spec.cwd, kind: spec.kind })
    }

    fn set_plugins(&self, plugins: Vec<crate::plugin::Plugin>) {
        *self.plugin_servers.lock().unwrap() = plugins
            .iter()
            .flat_map(|p| p.mcp_servers.iter())
            .map(|s| {
                let headers =
                    || s.headers.iter().map(|(k, v)| HttpHeader::new(k, v)).collect::<Vec<_>>();
                if s.url.is_empty() {
                    let env = s.env.iter().map(|(k, v)| EnvVariable::new(k, v)).collect();
                    McpServer::Stdio(
                        McpServerStdio::new(s.name.clone(), s.command.clone())
                            .args(s.args.clone())
                            .env(env),
                    )
                } else if s.sse {
                    McpServer::Sse(
                        McpServerSse::new(s.name.clone(), s.url.clone()).headers(headers()),
                    )
                } else {
                    McpServer::Http(
                        McpServerHttp::new(s.name.clone(), s.url.clone()).headers(headers()),
                    )
                }
            })
            .collect();
    }

    fn resume_session(
        &self,
        token: u64,
        session_id: &str,
        spec: crate::SessionSpec,
        _history: &[crate::Record],
    ) -> anyhow::Result<()> {
        // agent 自己保有上下文（session/resume 或 session/load），本地日志用不上
        self.send_command(KernelCommand::ResumeSession {
            token,
            session_id: session_id.to_string(),
            cwd: spec.cwd,
            kind: spec.kind,
        })
    }

    fn send_prompt(&self, session: &str, prompt: crate::UserPrompt) -> anyhow::Result<()> {
        self.send_command(KernelCommand::Prompt { session: session.to_string(), prompt })
    }

    fn cancel(&self, session: &str) -> anyhow::Result<()> {
        self.elicitation
            .lock()
            .unwrap()
            .retain(|_, (session_id, _)| session_id.as_deref() != Some(session));
        self.send_command(KernelCommand::Cancel { session: session.to_string() })
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

    fn resolve_question(
        &self,
        request_id: u64,
        answer: crate::QuestionAnswer,
    ) -> anyhow::Result<()> {
        let action = match answer {
            crate::QuestionAnswer::Form { values } => {
                let content: std::collections::BTreeMap<_, _> = values
                    .into_iter()
                    .map(|answer| {
                        let value = match answer.value {
                            crate::FormValueKind::Text(text) => {
                                ElicitationContentValue::String(text)
                            }
                            crate::FormValueKind::Number(number) => {
                                ElicitationContentValue::Number(number)
                            }
                            crate::FormValueKind::Integer(number) => {
                                ElicitationContentValue::Integer(number)
                            }
                            crate::FormValueKind::Boolean(value) => {
                                ElicitationContentValue::Boolean(value)
                            }
                            crate::FormValueKind::Strings(values) => {
                                ElicitationContentValue::StringArray(values)
                            }
                        };
                        (answer.id, value)
                    })
                    .collect();
                ElicitationAction::Accept(
                    agent_client_protocol::schema::v1::ElicitationAcceptAction::new()
                        .content(content),
                )
            }
            crate::QuestionAnswer::ModelDecides | crate::QuestionAnswer::Choices { .. } => {
                ElicitationAction::Decline
            }
            crate::QuestionAnswer::SelfAnswer | crate::QuestionAnswer::Cancelled => {
                ElicitationAction::Cancel
            }
        };
        let (_, (_, sender)) = self
            .elicitation
            .lock()
            .unwrap()
            .remove_entry(&request_id)
            .ok_or_else(|| anyhow::anyhow!("unknown elicitation request {request_id}"))?;
        sender
            .send(action)
            .map_err(|_| anyhow::anyhow!("elicitation request {request_id} already closed"))
    }

    fn set_mode(&self, session: &str, mode_id: &str) -> anyhow::Result<()> {
        self.send_command(KernelCommand::SetMode {
            session: session.to_string(),
            mode_id: mode_id.to_string(),
        })
    }

    fn set_model(&self, session: &str, model: &str) -> anyhow::Result<()> {
        self.send_command(KernelCommand::SetModel {
            session: session.to_string(),
            model: model.to_string(),
        })
    }

    fn set_thinking(&self, session: &str, level: Option<&str>) -> anyhow::Result<()> {
        // ACP 没有「回到 agent 默认档」这条路：只能选一个具体档位
        let level =
            level.ok_or_else(|| anyhow::anyhow!("ACP agent 没有默认档位，请选一个具体档位"))?;
        self.send_command(KernelCommand::SetThinking {
            session: session.to_string(),
            level: level.to_string(),
        })
    }

    fn authenticate(&self, method_id: &str) -> anyhow::Result<()> {
        self.send_command(KernelCommand::Authenticate { method_id: method_id.to_string() })
    }

    fn can_idle_shutdown(&self) -> bool {
        self.resumable.load(Ordering::SeqCst)
    }

    fn shutdown(&self) {
        for (_, (_, sender)) in self.elicitation.lock().unwrap().drain() {
            let _ = sender.send(ElicitationAction::Cancel);
        }
        let _ = self.command_tx.unbounded_send(KernelCommand::Shutdown);
    }
}

/// 内核线程主体。
fn run_kernel(
    config: AcpKernelConfig,
    mut cmd_rx: futures::channel::mpsc::UnboundedReceiver<KernelCommand>,
    sink: EventSink,
    pending: PendingPermissions,
    plugin_servers: PluginServers,
    resumable: Arc<AtomicBool>,
    elicitation: PendingElicitations,
) {
    let sink_fatal = sink.clone();
    let result = smol::block_on(async move {
        let mut agent = AcpAgent::from_str(&config.command)
            .map_err(|e| anyhow::anyhow!("parse agent command: {e}"))?;
        if !config.env.is_empty() {
            agent = AcpAgent::new(agent.into_config().envs(config.env.iter().cloned()));
        }
        if config.debug_wire {
            agent = agent.with_debug(|line, dir| eprintln!("[wire {dir:?}] {line}"));
        }

        // ---- handlers ------------------------------------------------

        let sink_updates = sink.clone();
        let sink_permission = sink.clone();
        let sink_fs_read = sink.clone();
        let sink_fs_write = sink.clone();
        let sink_elicitation = sink.clone();
        let pending_elicitation = elicitation.clone();

        // session/load 期间 agent 会重放历史；本地已有记录，这些重放直接丢弃。
        // 直到该会话发出下一轮提示才解除（不依赖通知与响应的到达顺序）。
        let replaying: Arc<Mutex<HashSet<String>>> = Arc::default();
        let replaying_updates = replaying.clone();
        let shared = Arc::new(AcpShared::default());
        let shared_updates = shared.clone();

        let default_cwd = config.cwd.clone();
        let auto_approve = config.auto_approve;
        let next_request_id = Arc::new(AtomicU64::new(1));
        let next_elicitation_id = next_request_id.clone();

        Client
            .builder()
            .on_receive_notification(
                async move |notification: SessionNotification, _connection| {
                    let sid = notification.session_id.to_string();
                    if replaying_updates.lock().unwrap().contains(&sid) {
                        return Ok(());
                    }
                    if !shared_updates.started.lock().unwrap().contains(&sid) {
                        shared_updates.early.lock().unwrap().push(notification);
                        return Ok(());
                    }
                    map_notification(&sink_updates, &notification, &shared_updates);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: RequestPermissionRequest, responder, connection| {
                    let session = request.session_id.to_string();
                    let tool = Some(request.tool_call.tool_call_id.to_string());
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
                            KernelEvent::PermissionRequest {
                                id,
                                title,
                                options,
                                tool,
                                hunks: None,
                            },
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
                                    tool,
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
                                    tool,
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
                    if request.line == Some(0) {
                        emit(
                            &sink_fs_read,
                            Some(session),
                            KernelEvent::FsRead {
                                path: format!("{path} (line 从 1 开始)"),
                                ok: false,
                            },
                        );
                        return responder.respond_with_error(
                            agent_client_protocol::Error::invalid_params().data("line 从 1 开始"),
                        );
                    }
                    let result = std::fs::read_to_string(&request.path).map_err(|err| {
                        if err.kind() == std::io::ErrorKind::NotFound {
                            agent_client_protocol::Error::resource_not_found(Some(path.clone()))
                        } else {
                            agent_client_protocol::Error::into_internal_error(&err)
                        }
                    });
                    match result {
                        Ok(content) => {
                            let content = read_text_range(&content, request.line, request.limit);
                            emit(
                                &sink_fs_read,
                                Some(session),
                                KernelEvent::FsRead { path, ok: true },
                            );
                            responder.respond(ReadTextFileResponse::new(content))
                        }
                        Err(err) => {
                            emit(
                                &sink_fs_read,
                                Some(session),
                                KernelEvent::FsRead {
                                    path: format!("{path} ({})", err.message),
                                    ok: false,
                                },
                            );
                            responder.respond_with_error(err)
                        }
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: WriteTextFileRequest, responder, _connection| {
                    let session = request.session_id.to_string();
                    let path = request.path.to_string_lossy().into_owned();
                    // 检查点：写盘的是我们，前后各读一次交给 UI（回退时还原）。
                    // agent 自己直接写盘的看不见
                    let before = crate::checkpoint::read_snapshot(&request.path);
                    let result = std::fs::write(&request.path, &request.content);
                    if result.is_ok()
                        && let Some(before) = before
                        && let Some(after) = crate::checkpoint::read_snapshot(&request.path)
                        && before != after
                    {
                        let change = KernelEvent::FileChanged { path: path.clone(), before, after };
                        emit(&sink_fs_write, Some(session.clone()), change);
                    }
                    match result {
                        Ok(()) => {
                            emit(
                                &sink_fs_write,
                                Some(session),
                                KernelEvent::FsWrite { path, ok: true },
                            );
                            responder.respond(WriteTextFileResponse::new())
                        }
                        Err(err) => {
                            emit(
                                &sink_fs_write,
                                Some(session),
                                KernelEvent::FsWrite { path, ok: false },
                            );
                            responder.respond_with_error(
                                agent_client_protocol::Error::into_internal_error(&err),
                            )
                        }
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: CreateElicitationRequest, responder, connection| {
                    let session = match request.scope() {
                        ElicitationScope::Session(scope) => Some(scope.session_id.to_string()),
                        ElicitationScope::Request(_) => None,
                        _ => None,
                    };
                    let questions = match elicitation_questions(&request) {
                        Ok(questions) => questions,
                        Err(error) => {
                            if let Some(session) = session {
                                emit(
                                    &sink_elicitation,
                                    Some(session),
                                    KernelEvent::Notice(format!("无法显示提问：{}", error.message)),
                                );
                            }
                            return responder.respond_with_error(error);
                        }
                    };
                    let id = next_elicitation_id.fetch_add(1, Ordering::Relaxed);
                    let (tx, rx) = oneshot::channel::<ElicitationAction>();
                    pending_elicitation.lock().unwrap().insert(id, (session.clone(), tx));
                    emit(
                        &sink_elicitation,
                        session,
                        KernelEvent::UserQuestion { id, tool: None, questions },
                    );
                    connection.spawn(async move {
                        let action = rx.await.unwrap_or(ElicitationAction::Cancel);
                        responder.respond(CreateElicitationResponse::new(action))
                    })
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(agent, move |connection: ConnectionTo<Agent>| async move {
                // ---- 握手 ------------------------------------------------
                let init = connection
                    .send_request(
                        InitializeRequest::new(ProtocolVersion::V1)
                            .client_capabilities(
                                ClientCapabilities::default()
                                    .fs(FileSystemCapabilities::new()
                                        .read_text_file(true)
                                        .write_text_file(true))
                                    .session(
                                        ClientSessionCapabilities::new()
                                            .compaction(CompactionCapabilities::new())
                                            .notices(NoticeCapabilities::new()),
                                    )
                                    .elicitation(
                                        ElicitationCapabilities::new()
                                            .form(ElicitationFormCapabilities::new()),
                                    ),
                            )
                            .client_info(Implementation::new(
                                "claudesktop",
                                env!("CARGO_PKG_VERSION"),
                            )),
                    )
                    .block_task()
                    .await?;
                let caps = &init.agent_capabilities;
                let prompt_images = caps.prompt_capabilities.image;
                // 远程 MCP 服务只交给声明了支持它的 agent
                let mcp_caps = caps.mcp_capabilities.clone();
                let supported = move |servers: Vec<McpServer>| -> Vec<McpServer> {
                    servers
                        .into_iter()
                        .filter(|s| match s {
                            McpServer::Http(_) => mcp_caps.http,
                            McpServer::Sse(_) => mcp_caps.sse,
                            _ => true,
                        })
                        .collect()
                };
                let can_resume = caps.session_capabilities.resume.is_some();
                let can_load = caps.load_session;
                resumable.store(can_resume || can_load, Ordering::SeqCst);
                let agent_name =
                    init.agent_info.map(|info| info.name).unwrap_or_else(|| "unknown-agent".into());
                let auth_methods = auth_views(&init.auth_methods);
                *shared.auth_methods.lock().unwrap() = auth_methods.clone();
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
                    KernelEvent::Connected { agent_name: agent_name.clone(), auth_methods },
                );

                // ---- 指令循环 --------------------------------------------
                while let Some(command) = cmd_rx.next().await {
                    match command {
                        KernelCommand::NewSession { token, cwd, kind } => {
                            // ACP agent 都是写代码的：聊天会话直接回绝（界面本来就不会这么发，这是兜底）
                            if kind == SessionKind::Chat {
                                emit(
                                    &sink,
                                    None,
                                    KernelEvent::SessionFailed {
                                        token,
                                        message: "这个 agent 不支持纯聊天".into(),
                                    },
                                );
                                continue;
                            }
                            let cwd = cwd.unwrap_or_else(|| default_cwd.clone());
                            let servers = supported(plugin_servers.lock().unwrap().clone());
                            let opened = open_new_session(&connection, cwd, servers).await;
                            report_started(&sink, &shared, &agent_name, token, opened, false);
                        }
                        KernelCommand::ResumeSession { token, session_id, cwd, kind } => {
                            if kind == SessionKind::Chat {
                                emit(
                                    &sink,
                                    None,
                                    KernelEvent::SessionFailed {
                                        token,
                                        message: "这个 agent 不支持纯聊天".into(),
                                    },
                                );
                                continue;
                            }
                            let cwd = cwd.unwrap_or_else(|| default_cwd.clone());
                            // 先拷出来再放锁：锁不能跨 await 拿着（界面线程 set_plugins 会被卡住）
                            let servers = supported(plugin_servers.lock().unwrap().clone());
                            let restored = if can_resume {
                                connection
                                    .send_request(
                                        ResumeSessionRequest::new(session_id.clone(), cwd.clone())
                                            .mcp_servers(servers.clone()),
                                    )
                                    .block_task()
                                    .await
                                    .map(|r| OpenedSession {
                                        session_id: session_id.clone(),
                                        model: current_model(&r.config_options),
                                        config_options: config_views(
                                            r.config_options.as_deref().unwrap_or_default(),
                                        ),
                                        modes: r.modes,
                                    })
                                    .map_err(|e| {
                                        SessionOpenError::new(format!("session/resume: {e}"))
                                    })
                            } else if can_load {
                                replaying.lock().unwrap().insert(session_id.clone());
                                let loaded = connection
                                    .send_request(
                                        LoadSessionRequest::new(session_id.clone(), cwd.clone())
                                            .mcp_servers(servers.clone()),
                                    )
                                    .block_task()
                                    .await;
                                if loaded.is_err() {
                                    replaying.lock().unwrap().remove(&session_id);
                                }
                                loaded
                                    .map(|r| OpenedSession {
                                        session_id: session_id.clone(),
                                        model: current_model(&r.config_options),
                                        config_options: config_views(
                                            r.config_options.as_deref().unwrap_or_default(),
                                        ),
                                        modes: r.modes,
                                    })
                                    .map_err(|e| {
                                        SessionOpenError::new(format!("session/load: {e}"))
                                    })
                            } else {
                                Err(SessionOpenError::new(
                                    "agent 不支持 session/resume 或 session/load",
                                ))
                            };
                            match restored {
                                Ok(opened) => report_started(
                                    &sink,
                                    &shared,
                                    &agent_name,
                                    token,
                                    Ok(opened),
                                    true,
                                ),
                                Err(reason) => {
                                    emit(
                                        &sink,
                                        None,
                                        KernelEvent::Status(format!(
                                            "无法续接会话 {session_id}，改为新会话：{}",
                                            reason.message
                                        )),
                                    );
                                    let opened = open_new_session(&connection, cwd, servers).await;
                                    report_started(
                                        &sink,
                                        &shared,
                                        &agent_name,
                                        token,
                                        opened,
                                        false,
                                    );
                                }
                            }
                        }
                        KernelCommand::Prompt { session, prompt } => {
                            replaying.lock().unwrap().remove(&session);
                            shared.prompted.lock().unwrap().insert(session.clone());
                            // 在独立任务里等本轮结束：指令循环必须保持空闲，
                            // 否则一轮进行中的 Cancel / SetMode / 其他会话的提示
                            // 都要等到轮末才处理。
                            let sink_turn = sink.clone();
                            let conn = connection.clone();
                            connection.spawn(async move {
                                if !prompt.images.is_empty() && !prompt_images {
                                    emit(
                                        &sink_turn,
                                        Some(session),
                                        KernelEvent::PromptFailed {
                                            message:
                                                "这个 agent 不支持图片输入；请移除图片附件后重试"
                                                    .into(),
                                        },
                                    );
                                    return Ok(());
                                }
                                // 文本 + 图片附件 → ACP 内容块
                                let mut blocks =
                                    vec![ContentBlock::Text(TextContent::new(prompt.text))];
                                for image in &prompt.images {
                                    blocks.push(ContentBlock::Image(ImageContent::new(
                                        image.data.clone(),
                                        image.media_type.clone(),
                                    )));
                                }
                                let response = conn
                                    .send_request(PromptRequest::new(session.clone(), blocks))
                                    .block_task()
                                    .await;
                                let event = match response {
                                    Ok(r) => KernelEvent::TurnEnded {
                                        stop_reason: wire_name(&r.stop_reason),
                                    },
                                    Err(err) => {
                                        KernelEvent::PromptFailed { message: format!("{err}") }
                                    }
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
                        KernelCommand::SetModel { session, model } => {
                            let options = shared.configs.lock().unwrap().get(&session).cloned();
                            let model_config = options.as_ref().and_then(|options| {
                                options.iter().find(|option| {
                                    option.category == "model" || option.id == "model"
                                })
                            });
                            if let Some(option) = model_config {
                                let result = connection
                                    .send_request(SetSessionConfigOptionRequest::new(
                                        session.clone(),
                                        option.id.clone(),
                                        model.as_str(),
                                    ))
                                    .block_task()
                                    .await;
                                match result {
                                    Ok(response) => {
                                        let mut options = config_views(&response.config_options);
                                        // agent 刷新配置时不会带上我们从 `modes` 合成的思考项
                                        merge_mode_thought(&shared, &session, &mut options);
                                        shared
                                            .configs
                                            .lock()
                                            .unwrap()
                                            .insert(session.clone(), options.clone());
                                        emit(
                                            &sink,
                                            Some(session),
                                            KernelEvent::ConfigOptionsChanged { options },
                                        );
                                    }
                                    Err(err) => {
                                        emit(
                                            &sink,
                                            Some(session.clone()),
                                            KernelEvent::Status(format!("切换模型失败: {err}")),
                                        );
                                        if let Some(options) = options {
                                            emit(
                                                &sink,
                                                Some(session),
                                                KernelEvent::ConfigOptionsChanged { options },
                                            );
                                        }
                                    }
                                }
                            } else {
                                emit(
                                    &sink,
                                    Some(session),
                                    KernelEvent::Status("agent 未提供模型配置".into()),
                                );
                            }
                        }
                        KernelCommand::SetThinking { session, level } => {
                            let thought = session_configs(&shared, &session)
                                .into_iter()
                                .find(crate::is_selectable_thought);
                            match thought {
                                // 原生 `thought_level`：走 ACP 的配置项通道
                                Some(option) if option.id != MODE_THOUGHT_ID => {
                                    let result = connection
                                        .send_request(SetSessionConfigOptionRequest::new(
                                            session.clone(),
                                            option.id.clone(),
                                            level.as_str(),
                                        ))
                                        .block_task()
                                        .await;
                                    match result {
                                        Ok(response) => {
                                            let mut options =
                                                config_views(&response.config_options);
                                            merge_mode_thought(&shared, &session, &mut options);
                                            shared
                                                .configs
                                                .lock()
                                                .unwrap()
                                                .insert(session.clone(), options.clone());
                                            emit(
                                                &sink,
                                                Some(session),
                                                KernelEvent::ConfigOptionsChanged { options },
                                            );
                                        }
                                        Err(err) => {
                                            emit(
                                                &sink,
                                                Some(session.clone()),
                                                KernelEvent::Status(format!(
                                                    "切换思考强度失败: {err}"
                                                )),
                                            );
                                            // 回滚：UI 上还是原来那档（以当下配置表为准）
                                            emit(
                                                &sink,
                                                Some(session.clone()),
                                                KernelEvent::ConfigOptionsChanged {
                                                    options: session_configs(&shared, &session),
                                                },
                                            );
                                        }
                                    }
                                }
                                // 思考强度塞在 `modes` 里的 agent：换档就是换 mode
                                Some(_) => {
                                    let result = connection
                                        .send_request(SetSessionModeRequest::new(
                                            session.clone(),
                                            level.clone(),
                                        ))
                                        .block_task()
                                        .await;
                                    match result {
                                        Ok(_) => {
                                            // 重新取当下配置表（`.await` 期间可能已被刷新）
                                            let mut options = session_configs(&shared, &session);
                                            if let Some(slot) = options
                                                .iter_mut()
                                                .find(|option| option.id == MODE_THOUGHT_ID)
                                            {
                                                slot.current_value = level.clone();
                                            }
                                            if let Some(synthesized) = shared
                                                .mode_thought
                                                .lock()
                                                .unwrap()
                                                .get_mut(&session)
                                            {
                                                synthesized.current_value = level.clone();
                                            }
                                            shared
                                                .configs
                                                .lock()
                                                .unwrap()
                                                .insert(session.clone(), options.clone());
                                            emit(
                                                &sink,
                                                Some(session.clone()),
                                                KernelEvent::ConfigOptionsChanged { options },
                                            );
                                            emit(
                                                &sink,
                                                Some(session),
                                                KernelEvent::Status(format!(
                                                    "思考强度已切换: {level}"
                                                )),
                                            );
                                        }
                                        Err(err) => {
                                            emit(
                                                &sink,
                                                Some(session.clone()),
                                                KernelEvent::Status(format!(
                                                    "切换思考强度失败: {err}"
                                                )),
                                            );
                                            emit(
                                                &sink,
                                                Some(session.clone()),
                                                KernelEvent::ConfigOptionsChanged {
                                                    options: session_configs(&shared, &session),
                                                },
                                            );
                                        }
                                    }
                                }
                                None => emit(
                                    &sink,
                                    Some(session),
                                    KernelEvent::Status("agent 未提供思考强度配置".into()),
                                ),
                            }
                        }
                        KernelCommand::Authenticate { method_id } => {
                            let method = shared
                                .auth_methods
                                .lock()
                                .unwrap()
                                .iter()
                                .find(|method| method.id == method_id)
                                .cloned();
                            match method {
                                Some(method) => match method.kind {
                                    AuthMethodKind::Agent => {
                                        let conn = connection.clone();
                                        let sink_login = sink.clone();
                                        connection.spawn(async move {
                                            let result = conn
                                                .send_request(AuthenticateRequest::new(
                                                    method_id.clone(),
                                                ))
                                                .block_task()
                                                .await;
                                            let (success, message) = match result {
                                                Ok(_) => (true, "登录成功".into()),
                                                Err(err) => (false, format!("登录失败: {err}")),
                                            };
                                            emit(
                                                &sink_login,
                                                None,
                                                KernelEvent::AuthCompleted {
                                                    method_id,
                                                    success,
                                                    message,
                                                },
                                            );
                                            Ok(())
                                        })?;
                                    }
                                    AuthMethodKind::Terminal { .. } => {
                                        match spawn_terminal_login(&config.command, &method) {
                                            Ok(mut child) => {
                                                let sink_login = sink.clone();
                                                connection.spawn(async move {
                                                    let (success, message) =
                                                        match smol::unblock(move || child.wait())
                                                            .await
                                                        {
                                                            Ok(status) if status.success() => {
                                                                (true, "登录成功".into())
                                                            }
                                                            Ok(status) => (
                                                                false,
                                                                format!("登录进程已退出：{status}"),
                                                            ),
                                                            Err(err) => (
                                                                false,
                                                                format!("等待登录进程失败: {err}"),
                                                            ),
                                                        };
                                                    emit(
                                                        &sink_login,
                                                        None,
                                                        KernelEvent::AuthCompleted {
                                                            method_id,
                                                            success,
                                                            message,
                                                        },
                                                    );
                                                    Ok(())
                                                })?;
                                            }
                                            Err(message) => emit(
                                                &sink,
                                                None,
                                                KernelEvent::AuthCompleted {
                                                    method_id,
                                                    success: false,
                                                    message: message.to_string(),
                                                },
                                            ),
                                        }
                                    }
                                },
                                None => emit(
                                    &sink,
                                    None,
                                    KernelEvent::AuthCompleted {
                                        method_id,
                                        success: false,
                                        message: "agent 未提供这种登录方式".into(),
                                    },
                                ),
                            }
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

#[derive(Default)]
struct ToolContentView {
    text: String,
    images: Vec<crate::PromptImage>,
}

/// 把 ACP `session/update` 通知映射为 [`KernelEvent`]。
fn map_notification(sink: &EventSink, notification: &SessionNotification, shared: &AcpShared) {
    let session = notification.session_id.to_string();
    let (tool_id, content, raw_output) = match &notification.update {
        SessionUpdate::ToolCall(call) => {
            (call.tool_call_id.to_string(), Some(call.content.as_slice()), call.raw_output.as_ref())
        }
        SessionUpdate::ToolCallUpdate(update) => (
            update.tool_call_id.to_string(),
            update.fields.content.as_deref(),
            update.fields.raw_output.as_ref(),
        ),
        _ => (String::new(), None, None),
    };
    let structured = content.map(tool_content_view);
    let event = match &notification.update {
        SessionUpdate::UserMessageChunk(chunk) => {
            text_of(&chunk.content).map(|text| KernelEvent::UserMessageChunk { text })
        }
        SessionUpdate::AgentMessageChunk(chunk) => text_of(&chunk.content).map(|text| {
            // 不是回复的正文降级成淡色提示，别混进助手气泡：
            // 1. 首轮 Prompt 之前到达的（新会话开机清单）；
            // 2. pi-acp 的开机清单可能晚于 Prompt 到达（`setTimeout(..., 0)`），
            //    对它第一条、以特征开头的那块单独认。
            let first = shared.saw_chunk.lock().unwrap().insert(session.clone());
            let pre_prompt = !shared.prompted.lock().unwrap().contains(&session);
            let startup = first
                && shared.agent_name.lock().unwrap().eq_ignore_ascii_case("pi-acp")
                && (text.starts_with("## Context") || text.starts_with("pi v"));
            if pre_prompt || startup {
                KernelEvent::Notice(text)
            } else {
                KernelEvent::AgentMessageChunk { text }
            }
        }),
        SessionUpdate::AgentThoughtChunk(chunk) => {
            text_of(&chunk.content).map(|text| KernelEvent::AgentThoughtChunk { text })
        }
        SessionUpdate::ToolCall(call) => Some(KernelEvent::ToolCallStarted {
            id: call.tool_call_id.to_string(),
            title: call.title.clone(),
            status: wire_name(&call.status),
            kind: wire_name(&call.kind),
            parent: None,
        }),
        SessionUpdate::ToolCallUpdate(update) => {
            let text = raw_output.map(output_text);
            Some(KernelEvent::ToolCallUpdated {
                id: update.tool_call_id.to_string(),
                title: update.fields.title.clone(),
                status: update.fields.status.as_ref().map(wire_name),
                raw_output: text.as_deref().map(|t| truncate(t, 2000)),
                output: text.as_deref().map(crate::output_preview),
            })
        }
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
        SessionUpdate::AvailableCommandsUpdate(update) => {
            Some(KernelEvent::AvailableCommandsChanged {
                commands: update
                    .available_commands
                    .iter()
                    .map(|command| AvailableCommandView {
                        name: command.name.clone(),
                        description: command.description.clone(),
                        requires_input: command.input.is_some(),
                    })
                    .collect(),
            })
        }
        SessionUpdate::CurrentModeUpdate(update) => {
            let mode_id = update.current_mode_id.to_string();
            // 思考型 `modes`：改档位等价于改 mode，合成回 `thought_level` 配置项
            if shared.thinking_mode_sessions.lock().unwrap().contains(&session) {
                let updated = {
                    let mut synthesized = shared.mode_thought.lock().unwrap();
                    synthesized.get_mut(&session).map(|thought| {
                        thought.current_value = mode_id;
                        thought.clone()
                    })
                };
                match updated {
                    Some(thought) => {
                        let mut options = shared
                            .configs
                            .lock()
                            .unwrap()
                            .get(&session)
                            .cloned()
                            .unwrap_or_default();
                        match options.iter_mut().find(|option| option.id == MODE_THOUGHT_ID) {
                            Some(slot) => *slot = thought,
                            None => options.push(thought),
                        }
                        shared.configs.lock().unwrap().insert(session.clone(), options.clone());
                        Some(KernelEvent::ConfigOptionsChanged { options })
                    }
                    // 这个 agent 另报了原生 thought_level：交给下面的 ConfigOptionUpdate
                    None => None,
                }
            } else {
                Some(KernelEvent::CurrentModeChanged { mode_id })
            }
        }
        SessionUpdate::ConfigOptionUpdate(update) => {
            let mut options = config_views(&update.config_options);
            merge_mode_thought(shared, &session, &mut options);
            shared.configs.lock().unwrap().insert(session.clone(), options.clone());
            Some(KernelEvent::ConfigOptionsChanged { options })
        }
        SessionUpdate::SessionInfoUpdate(update) => match &update.title {
            MaybeUndefined::Value(title) => {
                Some(KernelEvent::TitleGenerated { title: title.clone() })
            }
            MaybeUndefined::Null => Some(KernelEvent::TitleGenerated { title: "新会话".into() }),
            MaybeUndefined::Undefined => None,
        },
        SessionUpdate::UsageUpdate(update) => Some(KernelEvent::ContextUsageUpdated {
            usage: ContextUsageView {
                used: update.used,
                size: update.size,
                cost: update
                    .cost
                    .as_ref()
                    .map(|cost| CostView { amount: cost.amount, currency: cost.currency.clone() }),
            },
        }),
        SessionUpdate::Notice(notice) => {
            let severity = match &notice.severity {
                agent_client_protocol::schema::v1::NoticeSeverity::Info => "通知",
                agent_client_protocol::schema::v1::NoticeSeverity::Warning => "警告",
                agent_client_protocol::schema::v1::NoticeSeverity::Error => "错误",
                agent_client_protocol::schema::v1::NoticeSeverity::Other(_) => "通知",
                _ => "通知",
            };
            let description =
                notice.description.as_deref().map(|text| format!("\n{text}")).unwrap_or_default();
            Some(KernelEvent::Notice(format!("{severity}：{}{description}", notice.title)))
        }
        SessionUpdate::CompactionUpdate(update) => {
            let id = update.compaction_id.to_string();
            let mut compactions = shared.compactions.lock().unwrap();
            let compaction =
                compactions.entry((session.clone(), id.clone())).or_insert_with(|| {
                    CompactionView {
                        id: id.clone(),
                        status: "in_progress".into(),
                        summary: None,
                        error: None,
                    }
                });
            compaction.status = wire_name(&update.status);
            match &update.summary {
                MaybeUndefined::Value(blocks) => {
                    compaction.summary = Some(content_summary(blocks));
                }
                MaybeUndefined::Null => compaction.summary = None,
                MaybeUndefined::Undefined => {}
            }
            match &update.error {
                MaybeUndefined::Value(error) => compaction.error = Some(error.clone()),
                MaybeUndefined::Null => compaction.error = None,
                MaybeUndefined::Undefined => {}
            }
            Some(KernelEvent::CompactionUpdated { compaction: compaction.clone() })
        }
        SessionUpdate::CompactionSummaryChunk(chunk) => {
            let id = chunk.compaction_id.to_string();
            let mut compactions = shared.compactions.lock().unwrap();
            let compaction =
                compactions.entry((session.clone(), id.clone())).or_insert_with(|| {
                    CompactionView {
                        id: id.clone(),
                        status: "in_progress".into(),
                        summary: None,
                        error: None,
                    }
                });
            compaction.status = "in_progress".into();
            let (text, _) = content_block_view(&chunk.content);
            if !text.is_empty() {
                match &mut compaction.summary {
                    Some(summary) => {
                        if !summary.is_empty() {
                            summary.push_str("\n\n");
                        }
                        summary.push_str(&text);
                    }
                    None => compaction.summary = Some(text),
                }
            }
            Some(KernelEvent::CompactionUpdated { compaction: compaction.clone() })
        }
        other => Some(KernelEvent::Notice(format!(
            "暂不支持的会话更新：{}",
            truncate(&raw_json(other), 4000)
        ))),
    };
    if let Some(event) = event {
        emit(sink, Some(session.clone()), event);
    }

    if let SessionUpdate::ToolCall(call) = &notification.update {
        let raw = raw_output.map(output_text);
        if raw.is_some() {
            emit(
                sink,
                Some(session.clone()),
                KernelEvent::ToolCallUpdated {
                    id: call.tool_call_id.to_string(),
                    title: None,
                    status: None,
                    raw_output: raw.as_deref().map(|t| truncate(t, 2000)),
                    output: raw.as_deref().map(crate::output_preview),
                },
            );
        }
    }

    let images_changed = match &notification.update {
        SessionUpdate::ToolCall(call) => !call.content.is_empty(),
        SessionUpdate::ToolCallUpdate(update) => update.fields.content.is_some(),
        _ => false,
    };
    if images_changed {
        let view = structured.unwrap_or_default();
        emit(
            sink,
            Some(session.clone()),
            KernelEvent::ToolCallContent {
                id: tool_id.clone(),
                output: crate::output_preview(&view.text),
            },
        );
        emit(
            sink,
            Some(session.clone()),
            KernelEvent::ToolCallImages { id: tool_id.clone(), images: view.images },
        );
    }

    for item in content.into_iter().flatten() {
        if let ToolCallContent::Diff(diff) = item {
            let view = DiffView::compute(
                diff.path.display().to_string(),
                diff.old_text.as_deref(),
                &diff.new_text,
            );
            emit(
                sink,
                Some(session.clone()),
                KernelEvent::ToolCallDiff { id: tool_id.clone(), diff: view },
            );
        }
    }
}

fn tool_content_view(content: &[ToolCallContent]) -> ToolContentView {
    let mut view = ToolContentView::default();
    for item in content {
        match item {
            ToolCallContent::Content(content) => {
                let (text, images) = content_block_view(&content.content);
                push_tool_text(&mut view.text, &text);
                view.images.extend(images);
            }
            ToolCallContent::Diff(_) => {}
            ToolCallContent::Terminal(terminal) => {
                push_tool_text(
                    &mut view.text,
                    &format!("终端：{}（实时终端将在后续里程碑接入）", terminal.terminal_id),
                );
            }
            other => push_tool_text(&mut view.text, &raw_json(other)),
        }
    }
    view
}

fn content_block_view(content: &ContentBlock) -> (String, Vec<crate::PromptImage>) {
    match content {
        ContentBlock::Text(text) => (text.text.clone(), Vec::new()),
        ContentBlock::Image(image) => (
            String::new(),
            vec![crate::PromptImage {
                media_type: image.mime_type.clone(),
                data: image.data.clone(),
            }],
        ),
        ContentBlock::Audio(audio) => {
            (format!("音频（{}）：暂不支持播放", audio.mime_type), Vec::new())
        }
        ContentBlock::ResourceLink(link) => {
            let mut text =
                format!("资源：{}\n{}", link.title.as_deref().unwrap_or(&link.name), link.uri);
            if let Some(description) = &link.description {
                text.push_str(&format!("\n{description}"));
            }
            (text, Vec::new())
        }
        ContentBlock::Resource(resource) => match &resource.resource {
            EmbeddedResourceResource::TextResourceContents(text) => {
                (format!("资源：{}\n\n{}", text.uri, text.text), Vec::new())
            }
            EmbeddedResourceResource::BlobResourceContents(blob) => {
                let mime = blob.mime_type.as_deref().unwrap_or("application/octet-stream");
                if mime.starts_with("image/") {
                    (
                        format!("图片资源：{}", blob.uri),
                        vec![crate::PromptImage {
                            media_type: mime.to_string(),
                            data: blob.blob.clone(),
                        }],
                    )
                } else {
                    (format!("二进制资源：{}（{mime}）", blob.uri), Vec::new())
                }
            }
            other => (raw_json(other), Vec::new()),
        },
        other => (raw_json(other), Vec::new()),
    }
}

fn content_summary(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| {
            let (text, images) = content_block_view(block);
            if !text.is_empty() {
                Some(text)
            } else {
                images.first().map(|image| format!("图片（{}）", image.media_type))
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn push_tool_text(text: &mut String, block: &str) {
    if block.is_empty() {
        return;
    }
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(block);
}

fn raw_json(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "无法序列化的内容".into())
}

struct SessionOpenError {
    message: String,
    auth_required: bool,
}

impl SessionOpenError {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), auth_required: false }
    }
}

impl From<agent_client_protocol::Error> for SessionOpenError {
    fn from(error: agent_client_protocol::Error) -> Self {
        Self {
            auth_required: error.code == agent_client_protocol::ErrorCode::AuthRequired,
            message: error.to_string(),
        }
    }
}

/// 新建 / 续接会话的共同结果。
struct OpenedSession {
    session_id: String,
    model: Option<String>,
    config_options: Vec<ConfigOptionView>,
    modes: Option<SessionModeState>,
}

async fn open_new_session(
    connection: &ConnectionTo<Agent>,
    cwd: PathBuf,
    mcp_servers: Vec<McpServer>,
) -> Result<OpenedSession, SessionOpenError> {
    connection
        .send_request(NewSessionRequest::new(cwd).mcp_servers(mcp_servers))
        .block_task()
        .await
        .map(|session| OpenedSession {
            session_id: session.session_id.to_string(),
            model: current_model(&session.config_options),
            config_options: config_views(session.config_options.as_deref().unwrap_or_default()),
            modes: session.modes,
        })
        .map_err(SessionOpenError::from)
}

/// 把会话打开结果报给 UI：成功 = `Started`，失败 = `SessionFailed`。
fn report_started(
    sink: &EventSink,
    shared: &AcpShared,
    agent_name: &str,
    token: u64,
    opened: Result<OpenedSession, SessionOpenError>,
    resumed: bool,
) {
    match opened {
        Ok(OpenedSession { session_id, model, mut config_options, modes }) => {
            *shared.agent_name.lock().unwrap() = agent_name.to_string();
            let (mut modes, mut current_mode) = match modes {
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
            // `modes` 报的是思考强度（pi-acp）：并进 `thought_level` 配置项，
            // 左侧权限模式留空，UI 那边只在模型菜单的 Effort 底栏显示
            if is_thinking_modes(&modes) {
                shared.thinking_mode_sessions.lock().unwrap().insert(session_id.clone());
                let has_native = config_options.iter().any(crate::is_selectable_thought);
                if has_native {
                    // 原生配置项优先，别两边各显示一个
                    shared.mode_thought.lock().unwrap().remove(&session_id);
                } else {
                    let thought = thought_option_from_modes(&modes, current_mode.as_deref());
                    shared.mode_thought.lock().unwrap().insert(session_id.clone(), thought);
                }
                modes.clear();
                current_mode = None;
            } else {
                shared.thinking_mode_sessions.lock().unwrap().remove(&session_id);
                shared.mode_thought.lock().unwrap().remove(&session_id);
            }
            merge_mode_thought(shared, &session_id, &mut config_options);
            shared.configs.lock().unwrap().insert(session_id.clone(), config_options.clone());
            shared.started.lock().unwrap().insert(session_id.clone());
            emit(
                sink,
                Some(session_id.clone()),
                KernelEvent::Started {
                    token,
                    agent_name: agent_name.to_string(),
                    session_id: session_id.clone(),
                    model,
                    modes,
                    current_mode,
                    resumed,
                },
            );
            emit(
                sink,
                Some(session_id.clone()),
                KernelEvent::ConfigOptionsChanged { options: config_options },
            );
            let early = {
                let mut pending = shared.early.lock().unwrap();
                let (early, remaining): (Vec<_>, Vec<_>) = pending
                    .drain(..)
                    .partition(|notification| notification.session_id.to_string() == session_id);
                *pending = remaining;
                early
            };
            for notification in early {
                map_notification(sink, &notification, shared);
            }
        }
        Err(error) if error.auth_required => emit(
            sink,
            None,
            KernelEvent::AuthRequired {
                token,
                methods: shared.auth_methods.lock().unwrap().clone(),
                message: error.message,
            },
        ),
        Err(error) => {
            emit(sink, None, KernelEvent::SessionFailed { token, message: error.message })
        }
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
        opt.get("currentValue").and_then(|v| v.as_str()).map(str::to_owned)
    })
}

struct LoginProcess {
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

#[cfg(windows)]
fn login_process(command: &str, method: &AuthMethodView) -> Result<LoginProcess, String> {
    let agent = AcpAgent::from_str(command).map_err(|err| err.to_string())?;
    let config = serde_json::to_value(agent.config()).map_err(|err| err.to_string())?;
    let program = config
        .get("command")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "启动命令缺少可执行程序".to_string())?;
    let mut args = config
        .get("args")
        .and_then(serde_json::Value::as_array)
        .map(|args| {
            args.iter().filter_map(|arg| arg.as_str().map(str::to_owned)).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let AuthMethodKind::Terminal { args: extra, env } = &method.kind else {
        return Err("终端登录参数不正确".into());
    };
    args.extend(extra.clone());
    let mut wrapped = vec!["/c".into(), "start".into(), "".into(), "/wait".into()];
    wrapped.push(program.to_string());
    wrapped.extend(args);
    Ok(LoginProcess { program: "cmd".into(), args: wrapped, env: env.clone() })
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn login_process(command: &str, method: &AuthMethodView) -> Result<LoginProcess, String> {
    let agent = AcpAgent::from_str(command).map_err(|err| err.to_string())?;
    let config = serde_json::to_value(agent.config()).map_err(|err| err.to_string())?;
    let program = config
        .get("command")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "启动命令缺少可执行程序".to_string())?;
    let mut args = config
        .get("args")
        .and_then(serde_json::Value::as_array)
        .map(|args| {
            args.iter().filter_map(|arg| arg.as_str().map(str::to_owned)).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let AuthMethodKind::Terminal { args: extra, env } = &method.kind else {
        return Err("终端登录参数不正确".into());
    };
    args.extend(extra.clone());
    let mut wrapped = vec!["-e".into(), program.to_string()];
    wrapped.extend(args);
    Ok(LoginProcess { program: "x-terminal-emulator".into(), args: wrapped, env: env.clone() })
}

#[cfg(target_os = "macos")]
fn login_process(_command: &str, _method: &AuthMethodView) -> Result<LoginProcess, String> {
    Err("macOS 终端登录请在终端里执行 agent 的登录命令".into())
}

fn spawn_terminal_login(
    command: &str,
    method: &AuthMethodView,
) -> std::io::Result<std::process::Child> {
    let process = login_process(command, method).map_err(std::io::Error::other)?;
    std::process::Command::new(process.program).args(process.args).envs(process.env).spawn()
}

fn elicitation_questions(
    request: &CreateElicitationRequest,
) -> Result<Vec<crate::QuestionView>, agent_client_protocol::Error> {
    let ElicitationMode::Form(form) = &request.mode else {
        return Err(
            agent_client_protocol::Error::method_not_found().data("URL elicitation 尚未实现")
        );
    };
    let schema = &form.requested_schema;
    schema
        .properties
        .iter()
        .map(|(id, property)| {
            let required = schema
                .required
                .as_ref()
                .is_some_and(|required| required.contains(id));
            let (kind, multi_select, options, default_value, title, description) = match property {
                ElicitationPropertySchema::String(string) => {
                    let options = if let Some(options) = &string.one_of {
                        options
                            .iter()
                            .map(|option| crate::QuestionOptionView {
                                label: option.title.clone(),
                                description: option.description.clone(),
                                value: Some(option.value.clone()),
                            })
                            .collect()
                    } else if let Some(values) = &string.enum_values {
                        values
                            .iter()
                            .map(|value| crate::QuestionOptionView {
                                label: value.clone(),
                                description: None,
                                value: Some(value.clone()),
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let kind = if options.is_empty() {
                        crate::QuestionKind::Text
                    } else {
                        crate::QuestionKind::Choice
                    };
                    (
                        kind,
                        false,
                        options,
                        string.default.clone(),
                        string.title.clone(),
                        string.description.clone(),
                    )
                }
                ElicitationPropertySchema::Number(number) => (
                    crate::QuestionKind::Number,
                    false,
                    Vec::new(),
                    number.default.map(|value| value.to_string()),
                    number.title.clone(),
                    number.description.clone(),
                ),
                ElicitationPropertySchema::Integer(integer) => (
                    crate::QuestionKind::Integer,
                    false,
                    Vec::new(),
                    integer.default.map(|value| value.to_string()),
                    integer.title.clone(),
                    integer.description.clone(),
                ),
                ElicitationPropertySchema::Boolean(boolean) => (
                    crate::QuestionKind::Boolean,
                    false,
                    vec![
                        crate::QuestionOptionView {
                            label: "是".into(),
                            description: None,
                            value: Some("true".into()),
                        },
                        crate::QuestionOptionView {
                            label: "否".into(),
                            description: None,
                            value: Some("false".into()),
                        },
                    ],
                    None,
                    boolean.title.clone(),
                    boolean.description.clone(),
                ),
                ElicitationPropertySchema::Array(array) => {
                    let options = match &array.items {
                        MultiSelectItems::String(items) => items
                            .values
                            .iter()
                            .map(|value| crate::QuestionOptionView {
                                label: value.clone(),
                                description: None,
                                value: Some(value.clone()),
                            })
                            .collect(),
                        MultiSelectItems::Titled(items) => items
                            .options
                            .iter()
                            .map(|option| crate::QuestionOptionView {
                                label: option.title.clone(),
                                description: option.description.clone(),
                                value: Some(option.value.clone()),
                            })
                            .collect(),
                        MultiSelectItems::Other(other) => {
                            return Err(agent_client_protocol::Error::invalid_params()
                                .data(raw_json(other)))
                        }
                        _ => {
                            return Err(agent_client_protocol::Error::invalid_params()
                                .data(raw_json(&array.items)))
                        }
                    };
                    (
                        crate::QuestionKind::Choice,
                        true,
                        options,
                        None,
                        array.title.clone(),
                        array.description.clone(),
                    )
                }
                other => {
                    return Err(
                        agent_client_protocol::Error::invalid_params().data(raw_json(other))
                    )
                }
            };
            let question = description
                .clone()
                .or_else(|| title.clone())
                .unwrap_or_else(|| id.clone());
            Ok(crate::QuestionView {
                question,
                header: title,
                options,
                id: Some(id.clone()),
                kind,
                multi_select,
                required,
                default_value,
            })
        })
        .collect()
}

fn auth_views(methods: &[AuthMethod]) -> Vec<AuthMethodView> {
    methods
        .iter()
        .filter_map(|method| {
            let kind = match method {
                AuthMethod::Agent(_) => AuthMethodKind::Agent,
                AuthMethod::Terminal(terminal) => AuthMethodKind::Terminal {
                    args: terminal.args.clone(),
                    env: terminal
                        .env
                        .iter()
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect(),
                },
                _ => return None,
            };
            Some(AuthMethodView {
                id: method.id().to_string(),
                name: method.name().to_string(),
                description: method.description().map(str::to_owned),
                kind,
            })
        })
        .collect()
}

fn config_views(
    options: &[agent_client_protocol::schema::v1::SessionConfigOption],
) -> Vec<ConfigOptionView> {
    options
        .iter()
        .map(|option| {
            let category = match &option.category {
                Some(SessionConfigOptionCategory::Mode) => "mode",
                Some(SessionConfigOptionCategory::Model) => "model",
                Some(SessionConfigOptionCategory::ModelConfig) => "model_config",
                Some(SessionConfigOptionCategory::ThoughtLevel) => "thought_level",
                Some(SessionConfigOptionCategory::Other(other)) => other.as_str(),
                Some(_) => "other",
                None => "",
            }
            .to_string();
            let (current_value, choices) = match &option.kind {
                SessionConfigKind::Select(select) => {
                    let current = select.current_value.to_string();
                    let choices = match &select.options {
                        SessionConfigSelectOptions::Ungrouped(options) => options
                            .iter()
                            .map(|option| ConfigChoiceView {
                                id: option.value.to_string(),
                                name: option.name.clone(),
                                description: option.description.clone(),
                                group: None,
                            })
                            .collect(),
                        SessionConfigSelectOptions::Grouped(groups) => groups
                            .iter()
                            .flat_map(|group| {
                                group.options.iter().map(|option| ConfigChoiceView {
                                    id: option.value.to_string(),
                                    name: option.name.clone(),
                                    description: option.description.clone(),
                                    group: Some(group.name.clone()),
                                })
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    (current, choices)
                }
                SessionConfigKind::Boolean(boolean) => {
                    (boolean.current_value.to_string(), Vec::new())
                }
                _ => (String::new(), Vec::new()),
            };
            ConfigOptionView {
                id: option.id.to_string(),
                name: option.name.clone(),
                description: option.description.clone(),
                category,
                current_value,
                options: choices,
            }
        })
        .collect()
}

/// 按 ACP 的 1 起始行号截取文本，保留原有的 LF / CRLF 和末行换行。
fn read_text_range(text: &str, line: Option<u32>, limit: Option<u32>) -> String {
    let start = line.unwrap_or(1).saturating_sub(1) as usize;
    let count = limit.map_or(usize::MAX, |n| n as usize);
    text.split_inclusive('\n').skip(start).take(count).collect()
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

    fn modes_of(ids: &[&str]) -> Vec<ModeView> {
        ids.iter()
            .map(|id| ModeView { id: (*id).into(), name: (*id).into(), description: None })
            .collect()
    }

    #[test]
    fn thinking_modes_are_recognized() {
        // pi-acp：思考强度
        assert!(is_thinking_modes(&modes_of(&[
            "off", "minimal", "low", "medium", "high", "xhigh"
        ])));
        // Claude Code / OpenCode / Gemini CLI：权限模式
        assert!(!is_thinking_modes(&modes_of(&[
            "default",
            "acceptEdits",
            "plan",
            "dontAsk",
            "bypassPermissions"
        ])));
        assert!(!is_thinking_modes(&modes_of(&["build", "plan"])));
        assert!(!is_thinking_modes(&modes_of(&["default", "autoEdit", "yolo", "plan"])));
        // 只有中性词：认不出是不是强度，仍留在左侧模式菜单
        assert!(!is_thinking_modes(&modes_of(&["default", "auto"])));
        assert!(!is_thinking_modes(&modes_of(&["off", "on"])));
        assert!(!is_thinking_modes(&modes_of(&["default", "off"])));
        // `extra_high` 也是强度档位
        assert!(is_thinking_modes(&modes_of(&["high", "extra_high"])));
        assert!(!is_thinking_modes(&[]));
    }

    fn model_option(value: &str) -> ConfigOptionView {
        ConfigOptionView {
            id: "model".into(),
            name: "模型".into(),
            description: None,
            category: "model".into(),
            current_value: value.into(),
            options: Vec::new(),
        }
    }

    #[test]
    fn synthesized_thought_option_survives_config_refresh() {
        let shared = AcpShared::default();
        let thought = thought_option_from_modes(&modes_of(&["off", "low", "high"]), Some("low"));
        assert_eq!(thought.category, "thought_level");
        assert_eq!(thought.current_value, "low");
        assert_eq!(thought.options.len(), 3);
        shared.mode_thought.lock().unwrap().insert("s1".into(), thought);

        // 换模型：agent 整表刷新，只回一个 model，合成项要合并回去
        let mut options = vec![model_option("model-b")];
        merge_mode_thought(&shared, "s1", &mut options);
        assert!(
            options.iter().any(|o| o.id == MODE_THOUGHT_ID && o.current_value == "low"),
            "{options:?}"
        );

        // 原生 thought_level（可选档位）出现了：合成项让位，两边只留一个
        options.push(ConfigOptionView {
            id: "thinking".into(),
            name: "思考强度".into(),
            description: None,
            category: "thought_level".into(),
            current_value: "high".into(),
            options: vec![
                ConfigChoiceView {
                    id: "low".into(),
                    name: "低".into(),
                    description: None,
                    group: None,
                },
                ConfigChoiceView {
                    id: "high".into(),
                    name: "高".into(),
                    description: None,
                    group: None,
                },
            ],
        });
        merge_mode_thought(&shared, "s1", &mut options);
        assert_eq!(options.iter().filter(|o| crate::is_thought_config(o)).count(), 1);
        assert!(shared.mode_thought.lock().unwrap().get("s1").is_none());
    }

    #[test]
    fn boolean_thought_config_does_not_displace_synthesized_effort() {
        // Boolean 型的 `thinking` 选不出具体档位：它既不能当 Effort 底栏，也不能把合成项挤掉
        let shared = AcpShared::default();
        let thought = thought_option_from_modes(&modes_of(&["off", "low", "high"]), Some("low"));
        shared.mode_thought.lock().unwrap().insert("s1".into(), thought);
        let mut options = vec![
            model_option("model-a"),
            ConfigOptionView {
                id: "thinking".into(),
                name: "思考强度".into(),
                description: None,
                category: "thought_level".into(),
                current_value: "true".into(),
                options: Vec::new(),
            },
        ];
        merge_mode_thought(&shared, "s1", &mut options);
        assert_eq!(options.iter().filter(|o| crate::is_selectable_thought(o)).count(), 1);
        assert!(options.iter().any(|o| o.id == MODE_THOUGHT_ID && o.current_value == "low"));
    }

    #[cfg(windows)]
    #[test]
    fn terminal_login_preserves_arguments_and_waits_for_exit() {
        let method = AuthMethodView {
            id: "terminal-login".into(),
            name: "终端登录".into(),
            description: None,
            kind: AuthMethodKind::Terminal {
                args: vec!["--terminal-login".into()],
                env: vec![("MOCK_AUTH".into(), "1".into())],
            },
        };
        let process = login_process("cmd /c npx -y mock-agent", &method).unwrap();
        assert_eq!(process.program, "cmd");
        assert_eq!(&process.args[..4], ["/c", "start", "", "/wait"]);
        assert!(process.args.contains(&"--terminal-login".to_string()));
        assert_eq!(process.env, vec![("MOCK_AUTH".into(), "1".into())]);
    }

    #[test]
    fn text_ranges_preserve_crlf_and_boundaries() {
        let text = "a\r\nb\r\nc\r\nd";
        assert_eq!(read_text_range(text, Some(2), Some(2)), "b\r\nc\r\n");
        assert_eq!(read_text_range(text, Some(4), None), "d");
        assert_eq!(read_text_range(text, Some(5), Some(1)), "");
        assert_eq!(read_text_range(text, Some(1), Some(0)), "");
        assert_eq!(read_text_range(text, None, None), text);
    }

    #[test]
    fn resource_content_is_readable_without_fetching() {
        let block: ContentBlock = serde_json::from_value(json!({
            "type": "resource_link",
            "name": "协议",
            "title": "ACP 文档",
            "uri": "https://example.com/acp",
            "description": "只显示"
        }))
        .unwrap();
        let (text, images) = content_block_view(&block);
        assert!(text.contains("ACP 文档"));
        assert!(text.contains("https://example.com/acp"));
        assert!(text.contains("只显示"));
        assert!(images.is_empty());
    }
}
