//! 用量网关：进程内的本地转发服务，自研内核发给模型端点的每个请求都经过它，每个请求记一行日志
//! （耗时、用量、重试、有没有被停掉）。设计见「想法-用量统计网关.md」。
//!
//! ```text
//! provider ──(http, 带 x-claudesktop-* 头)──▶ 127.0.0.1:<随机端口> ──(https)──▶ 真实端点
//!                                                   └─▶ 数据目录 gateway/<日期>.jsonl
//! ```
//!
//! - 只读观察：请求体原样转发，重试、压缩、参数都还在 provider 里；
//! - 响应边读边转（读到多少转多少、立刻刷出去，不攒），旁路解析 SSE 里的用量、结束原因、首个内容块；
//! - provider 停下（用户点停止）就断开连接，网关下一次往回写失败时断开上游，这条记成 `cancelled`；
//! - 连不上上游时回 502 并带 [`GATEWAY_ERROR`] 头，provider 据此报成和直连一样的「请求 … 失败」；
//! - 不记提示词正文、不记 key；日志按天分文件，超过 [`KEEP_DAYS`] 天或总量超过 [`KEEP_BYTES`] 删最旧的。
//!
//! 哪个会话、为什么发的请求由内核在线程上打标记（[`tag`]），provider 发请求时带上（见 [`route`]）。
//!
//! **外部 agent**（ACP 内核的子进程）加不了我们的请求头：启动时把它的 `ANTHROPIC_BASE_URL` /
//! `OPENAI_BASE_URL` 指到 `http://127.0.0.1:<端口>/via/<真实端点的 base64url>`（见 [`agent_env`]），
//! 网关从路径里取回真实端点，用途记成 `acp`。会话从请求体里认（Claude Code 在 `metadata.user_id`
//! 里带 `session_<ID>`，和 ACP 会话 ID 一致），第几轮由界面发消息时登记（[`note_turn`]）。

use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 真实端点（base URL）
const UPSTREAM: &str = "x-claudesktop-upstream";
const SESSION: &str = "x-claudesktop-session";
const PURPOSE: &str = "x-claudesktop-purpose";
const TURN: &str = "x-claudesktop-turn";
/// provider 的一次请求（重试共用）和第几次尝试
pub(crate) const REQUEST: &str = "x-claudesktop-request";
pub(crate) const ATTEMPT: &str = "x-claudesktop-attempt";
/// 网关自己没连上上游：502 响应带这个头，错误信息在响应体里
pub(crate) const GATEWAY_ERROR: &str = "x-claudesktop-gateway-error";
/// 日志留多少天 / 最多多大
const KEEP_DAYS: i64 = 30;
const KEEP_BYTES: u64 = 200 << 20;
/// 错误响应体最多留多少字节（取错误信息用）
const ERROR_BODY_MAX: usize = 64 << 10;
/// 外部 agent 的请求路径前缀：`/via/<真实端点的 base64url>/<原路径>`
const VIA: &str = "/via/";

/// 跑着的网关。
pub struct Gateway {
    port: u16,
    dir: PathBuf,
    /// 写日志时持有（各连接线程并发写同一个文件）
    log: Mutex<()>,
}

/// 网关的句柄：放进 [`crate::ProviderConfig`]，provider 据此把请求发给网关。
#[derive(Clone)]
pub struct GatewayHandle(Arc<Gateway>);

impl std::fmt::Debug for GatewayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Gateway(127.0.0.1:{})", self.0.port)
    }
}

impl PartialEq for GatewayHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl GatewayHandle {
    /// 日志目录
    pub fn dir(&self) -> &Path {
        &self.0.dir
    }
}

/// 默认日志目录：数据目录下的 `gateway/`。
pub fn default_dir() -> Option<PathBuf> {
    claudesktop_core::default_data_dir().ok().map(|d| d.join("gateway"))
}

/// 本进程共用的网关（第一次用时起，进程退出时随之结束）；起不来返回 None（provider 就直连）。
pub fn shared(dir: &Path) -> Option<GatewayHandle> {
    static SHARED: OnceLock<Mutex<Option<GatewayHandle>>> = OnceLock::new();
    let mut slot = SHARED.get_or_init(Mutex::default).lock().unwrap();
    if let Some(g) = slot.as_ref().filter(|g| g.dir() == dir) {
        return Some(g.clone());
    }
    let started = Gateway::start(dir.to_path_buf()).ok()?;
    *slot = Some(started.clone());
    Some(started)
}

impl Gateway {
    /// 在 127.0.0.1 的随机端口上起网关（每个连接一个线程），顺手清理过期日志。
    pub fn start(dir: PathBuf) -> std::io::Result<GatewayHandle> {
        std::fs::create_dir_all(&dir)?;
        prune(&dir, Local::now().date_naive());
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let gateway = Arc::new(Gateway { port, dir, log: Mutex::new(()) });
        // 上游连接池：同一端点的 TLS 连接复用
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .build()
            .new_agent();
        let g = gateway.clone();
        std::thread::Builder::new().name("usage-gateway".into()).spawn(move || {
            for client in listener.incoming().flatten() {
                let (g, agent) = (g.clone(), agent.clone());
                let _ = std::thread::Builder::new()
                    .name("usage-gateway-conn".into())
                    .spawn(move || g.serve(client, &agent));
            }
        })?;
        Ok(GatewayHandle(gateway))
    }

    fn serve(&self, mut client: TcpStream, agent: &ureq::Agent) {
        // 流式的小块要立刻发出去，不等 Nagle 攒包
        let _ = client.set_nodelay(true);
        let started = Instant::now();
        let Ok(request) = read_request(&client) else {
            return;
        };
        let mut entry = RequestLog::new(&request);
        let Some((upstream, path)) = upstream_of(&request) else {
            let _ = respond(&mut client, 400, &[], b"missing upstream");
            return;
        };
        entry.host = host_of(&upstream);
        let url = format!("{}{path}", upstream.trim_end_matches('/'));
        let headers = request.headers.iter().filter(|(name, _)| forwarded(name));
        let result = match request.method.as_str() {
            "GET" => {
                let mut builder = agent.get(&url);
                for (name, value) in headers {
                    builder = builder.header(name, value);
                }
                builder.call()
            }
            "POST" => {
                let mut builder = agent.post(&url);
                for (name, value) in headers {
                    builder = builder.header(name, value);
                }
                builder.send(&request.body[..])
            }
            _ => {
                let _ = respond(&mut client, 405, &[], b"method not allowed");
                return;
            }
        };
        let response = match result {
            Ok(response) => response,
            Err(err) => {
                let message = err.to_string();
                let body = json!({ "error": { "message": message } }).to_string();
                let _ = respond(&mut client, 502, &[(GATEWAY_ERROR, "1")], body.as_bytes());
                entry.error = Some(message);
                entry.total_ms = millis(started.elapsed());
                self.write(&entry, &request.body);
                return;
            }
        };
        entry.ttfb_ms = Some(millis(started.elapsed()));
        let status = response.status();
        entry.status = status.as_u16();
        let sse = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("event-stream"));
        let mut head = format!(
            "HTTP/1.1 {} {}\r\n",
            status.as_u16(),
            status.canonical_reason().unwrap_or("Status")
        );
        for (name, value) in response.headers() {
            if !hop_by_hop(name.as_str())
                && !matches!(name.as_str(), "content-length" | "content-encoding")
                && let Ok(value) = value.to_str()
            {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
        }
        head.push_str("Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
        let mut stats = StreamStats::new(entry.protocol == "anthropic");
        let mut error_body = Vec::new();
        let mut reader = response.into_body().into_reader();
        let mut buf = vec![0u8; 16 << 10];
        let mut broken = client.write_all(head.as_bytes()).is_err();
        while !broken {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = client.write_all(b"0\r\n\r\n");
                    break;
                }
                Ok(n) => {
                    let data = &buf[..n];
                    if sse {
                        stats.feed(data, started);
                    } else if !status.is_success() && error_body.len() < ERROR_BODY_MAX {
                        error_body.extend_from_slice(data);
                    }
                    broken = write_chunk(&mut client, data).is_err();
                }
                // 上游中途断了：不发结束块，让 provider 也看到断开（和直连时一样）
                Err(err) => {
                    entry.error = Some(format!("读上游失败: {err}"));
                    break;
                }
            }
        }
        entry.cancelled = broken;
        entry.total_ms = millis(started.elapsed());
        stats.fill(&mut entry);
        if !(200..300).contains(&entry.status) && entry.error.is_none() {
            let text = String::from_utf8_lossy(&error_body);
            entry.error = Some(crate::provider::error_message(&text));
        }
        self.write(&entry, &request.body);
    }

    /// 补上请求体里的模型、消息数、工具数，追加一行到当天的日志。
    fn write(&self, entry: &RequestLog, body: &[u8]) {
        let mut entry = entry.clone();
        if let Ok(value) = serde_json::from_slice::<Value>(body) {
            entry.model = value.get("model").and_then(Value::as_str).map(str::to_string);
            // 外部 agent：会话从请求体里认，第几轮查界面登记的
            if entry.session.is_none()
                && let Some(session) = value
                    .pointer("/metadata/user_id")
                    .and_then(Value::as_str)
                    .and_then(|id| id.rsplit_once("session_"))
                    .map(|(_, s)| s.to_string())
            {
                entry.turn = turn_of(&session);
                entry.session = Some(session);
            }
            entry.messages = value
                .get("messages")
                .or_else(|| value.get("input"))
                .and_then(Value::as_array)
                .map(Vec::len);
            entry.tools = value.get("tools").and_then(Value::as_array).map(Vec::len);
        }
        let Ok(line) = serde_json::to_string(&entry) else {
            return;
        };
        let path = self.dir.join(format!("{}.jsonl", Local::now().format("%Y-%m-%d")));
        let _guard = self.log.lock().unwrap();
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// 请求发到哪。
pub(crate) struct Route {
    /// 实际请求的地址（开了网关就是网关的）
    pub url: String,
    /// 真实端点的地址（报错时给人看）
    pub real: String,
    /// 发给网关的附加头（没开网关时为空）
    pub headers: Vec<(&'static str, String)>,
}

impl Route {
    pub fn via_gateway(&self) -> bool {
        !self.headers.is_empty()
    }
}

/// 开了网关就发给网关，真实端点和这个线程上的会话标记放进请求头；没开就直连。
pub(crate) fn route(gateway: Option<&GatewayHandle>, base_url: &str, path: &str) -> Route {
    let base = base_url.trim_end_matches('/');
    let real = format!("{base}{path}");
    let Some(g) = gateway else {
        return Route { url: real.clone(), real, headers: Vec::new() };
    };
    let mut headers = vec![(UPSTREAM, base.to_string())];
    TAG.with_borrow(|tag| {
        if let Some(tag) = tag {
            headers.push((SESSION, tag.session.clone()));
            headers.push((PURPOSE, tag.purpose.to_string()));
            headers.push((TURN, tag.turn.to_string()));
        }
    });
    Route { url: format!("http://127.0.0.1:{}{path}", g.0.port), real, headers }
}

/// 外部 agent 的请求经网关要设的环境变量：Anthropic / OpenAI 的 base URL 指向网关，
/// 真实端点取它原来的设置（没设就是官方地址），编码在路径里。
pub fn agent_env(gateway: &GatewayHandle) -> Vec<(String, String)> {
    [
        ("ANTHROPIC_BASE_URL", "https://api.anthropic.com"),
        ("OPENAI_BASE_URL", "https://api.openai.com/v1"),
    ]
    .into_iter()
    .map(|(key, default)| {
        let real = std::env::var(key).ok().filter(|v| !v.trim().is_empty());
        (key.to_string(), via_url(gateway, real.as_deref().unwrap_or(default)))
    })
    .collect()
}

/// 外部 agent 用的 base URL：经网关转到 `upstream`。
pub fn via_url(gateway: &GatewayHandle, upstream: &str) -> String {
    use base64::Engine as _;
    let encoded =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(upstream.trim_end_matches('/'));
    format!("http://127.0.0.1:{}{VIA}{encoded}", gateway.0.port)
}

/// 真实端点和转给它的路径：内核的请求在头里写着，外部 agent 的编码在路径里。
fn upstream_of(request: &Request) -> Option<(String, String)> {
    if let Some(upstream) = request.header(UPSTREAM) {
        return Some((upstream.to_string(), request.path.clone()));
    }
    use base64::Engine as _;
    let rest = request.path.strip_prefix(VIA)?;
    let (encoded, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let upstream = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded).ok()?;
    Some((String::from_utf8(upstream).ok()?, path.to_string()))
}

/// 外部 agent 的会话现在是第几轮（界面每发一条消息登记一次）。
static TURNS: OnceLock<Mutex<std::collections::HashMap<String, u64>>> = OnceLock::new();

/// 登记外部 agent 的会话 `session` 开始了第 `turn` 轮：之后这个会话的请求都记成这一轮。
pub fn note_turn(session: &str, turn: u64) {
    TURNS.get_or_init(Mutex::default).lock().unwrap().insert(session.to_string(), turn);
}

fn turn_of(session: &str) -> Option<u64> {
    TURNS.get()?.lock().unwrap().get(session).copied()
}

/// 线程上的请求标记（见 [`tag`]）。
struct Tag {
    session: String,
    purpose: &'static str,
    turn: u64,
}

thread_local! {
    static TAG: RefCell<Option<Tag>> = const { RefCell::new(None) };
}

/// 这个线程上接下来的请求属于哪个会话的第几轮、为什么发（turn / subagent / compact）；
/// guard 析构时恢复之前的。
pub(crate) fn tag(session: &str, purpose: &'static str, turn: u64) -> TagGuard {
    TagGuard(TAG.replace(Some(Tag { session: session.to_string(), purpose, turn })))
}

pub(crate) struct TagGuard(Option<Tag>);

impl Drop for TagGuard {
    fn drop(&mut self) {
        TAG.set(self.0.take());
    }
}

/// 日志里的一行（也是界面读日志时拿到的样子，见 [`read_logs`]）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestLog {
    /// 发出的时刻（Unix 毫秒）
    pub at_ms: u64,
    /// provider 的一次请求（重试共用）和第几次尝试
    #[serde(skip_serializing_if = "Option::is_none")]
    pub req_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// 内核会话 ID（子任务是 `主会话/调用 ID`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// 会话里的第几轮（同一轮的请求归到一起）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    /// turn / subagent / compact / models / other
    pub purpose: String,
    /// openai / anthropic
    pub protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub host: String,
    pub path: String,
    pub method: String,
    pub body_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<usize>,
    /// 上游的 HTTP 状态；连不上为 0
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    pub cancelled: bool,
}

impl RequestLog {
    fn new(request: &Request) -> Self {
        let via = request.path.starts_with(VIA);
        // 外部 agent 的路径去掉编码的端点那一段
        let path = upstream_of(request).map_or_else(|| request.path.clone(), |(_, p)| p);
        let path = path.split('?').next().unwrap_or_default().to_string();
        let purpose = request.header(PURPOSE).map(str::to_string).unwrap_or_else(|| {
            if path.ends_with("/models") {
                "models"
            } else if via {
                "acp"
            } else {
                "other"
            }
            .to_string()
        });
        RequestLog {
            at_ms: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
            req_id: request.header(REQUEST).and_then(|v| v.parse().ok()),
            attempt: request.header(ATTEMPT).and_then(|v| v.parse().ok()),
            session: request.header(SESSION).map(str::to_string),
            purpose,
            turn: request.header(TURN).and_then(|v| v.parse().ok()),
            protocol: if path.ends_with("/messages") { "anthropic" } else { "openai" }.into(),
            method: request.method.clone(),
            body_bytes: request.body.len(),
            path,
            ..RequestLog::default()
        }
    }
}

/// 旁路解析 SSE：按行拼，`data:` 行解析成 JSON 取用量、结束原因、首个内容块的时刻。
struct StreamStats {
    anthropic: bool,
    line: Vec<u8>,
    first_token_ms: Option<u64>,
    input: u64,
    output: u64,
    reasoning: u64,
    cache_read: u64,
    finish_reason: Option<String>,
}

impl StreamStats {
    fn new(anthropic: bool) -> Self {
        StreamStats {
            anthropic,
            line: Vec::new(),
            first_token_ms: None,
            input: 0,
            output: 0,
            reasoning: 0,
            cache_read: 0,
            finish_reason: None,
        }
    }

    fn feed(&mut self, data: &[u8], started: Instant) {
        for &byte in data {
            if byte != b'\n' {
                self.line.push(byte);
                continue;
            }
            let line = std::mem::take(&mut self.line);
            let text = String::from_utf8_lossy(&line);
            let Some(payload) = text.trim_end_matches('\r').strip_prefix("data:") else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(payload.trim()) else {
                continue;
            };
            let content = if self.anthropic {
                self.anthropic_event(&value)
            } else {
                self.openai_chunk(&value)
            };
            if content && self.first_token_ms.is_none() {
                self.first_token_ms = Some(millis(started.elapsed()));
            }
        }
    }

    /// 返回这一块有没有内容（正文、思考、工具调用）。口径同 `openai::parse_chunk`。
    fn openai_chunk(&mut self, value: &Value) -> bool {
        // Responses API（Codex 这类 agent 用）：用量在 response.completed 里，正文是 *.delta 事件
        if let Some(kind) = value.get("type").and_then(Value::as_str) {
            if let Some(usage) = value.pointer("/response/usage").filter(|u| u.is_object()) {
                let n = |p: &str| usage.pointer(p).and_then(Value::as_u64).unwrap_or(0);
                self.input = n("/input_tokens");
                self.output = n("/output_tokens");
                self.reasoning = n("/output_tokens_details/reasoning_tokens");
                self.cache_read = n("/input_tokens_details/cached_tokens");
                if let Some(status) = value.pointer("/response/status").and_then(Value::as_str) {
                    self.finish_reason = Some(status.to_string());
                }
            }
            return kind.ends_with(".delta");
        }
        if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
            let n = |p: &str| usage.pointer(p).and_then(Value::as_u64).unwrap_or(0);
            self.input = n("/prompt_tokens");
            self.output = n("/completion_tokens");
            self.reasoning = n("/completion_tokens_details/reasoning_tokens");
            self.cache_read =
                n("/prompt_tokens_details/cached_tokens").max(n("/prompt_cache_hit_tokens"));
        }
        let Some(choice) = value.pointer("/choices/0") else {
            return false;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.to_string());
        }
        let delta = &choice["delta"];
        let text = |k: &str| delta.get(k).and_then(Value::as_str).is_some_and(|t| !t.is_empty());
        text("content")
            || text("reasoning_content")
            || text("reasoning")
            || delta.get("tool_calls").and_then(Value::as_array).is_some_and(|c| !c.is_empty())
    }

    /// 口径同 `anthropic::parse_event`：输入 = 没命中缓存的 + 读缓存的 + 写缓存的。
    fn anthropic_event(&mut self, value: &Value) -> bool {
        match value.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                if let Some(u) = value.pointer("/message/usage") {
                    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                    self.input = n("input_tokens")
                        + n("cache_read_input_tokens")
                        + n("cache_creation_input_tokens");
                    self.cache_read = n("cache_read_input_tokens");
                }
                false
            }
            "message_delta" => {
                if let Some(n) = value.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    self.output = n;
                }
                if let Some(stop) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.finish_reason = Some(stop.to_string());
                }
                false
            }
            "content_block_start" | "content_block_delta" => true,
            _ => false,
        }
    }

    fn fill(&self, entry: &mut RequestLog) {
        entry.first_token_ms = self.first_token_ms;
        entry.input = self.input;
        entry.output = self.output;
        entry.reasoning = self.reasoning;
        entry.cache_read = self.cache_read;
        entry.finish_reason = self.finish_reason.clone();
    }
}

/// provider 发来的请求。
struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// 读请求行、头和（按 Content-Length 的）请求体。provider 只发这一种形状。
fn read_request(client: &TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(client);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let chunked = headers
        .iter()
        .any(|(n, v)| n == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        // 外部 agent 可能分块发请求体
        let mut body = Vec::new();
        loop {
            let mut size = String::new();
            reader.read_line(&mut size)?;
            let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            if size == 0 {
                // 跳过结尾的空行（不支持 trailer）
                let mut end = String::new();
                reader.read_line(&mut end)?;
                break;
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..])?;
            let mut crlf = [0u8; 2];
            reader.read_exact(&mut crlf)?;
        }
        body
    } else {
        let len = headers
            .iter()
            .find(|(n, _)| n == "content-length")
            .and_then(|(_, v)| v.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0; len];
        reader.read_exact(&mut body)?;
        body
    };
    let headers = headers.into_iter().filter(|(n, _)| n != "transfer-encoding").collect();
    Ok(Request { method, path, headers, body })
}

/// 转给上游的头：去掉逐跳的、长度（ureq 自己算）、压缩协商（ureq 自己管）和网关自己的标记。
fn forwarded(name: &str) -> bool {
    !hop_by_hop(name)
        && !matches!(name, "host" | "content-length" | "accept-encoding")
        && !name.starts_with("x-claudesktop-")
}

fn hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "upgrade"
            | "proxy-connection"
    )
}

/// 一次性回一个完整响应（网关自己的错误）。
fn respond(
    client: &mut TcpStream,
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    client.write_all(head.as_bytes())?;
    client.write_all(body)
}

/// 一个 chunked 块，拼成一次写（不拆成几个小包）。
fn write_chunk(client: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    let mut out = format!("{:x}\r\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
    client.write_all(&out)
}

/// 地址里的主机名（去掉协议、用户信息、路径）。
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or_default();
    authority.rsplit('@').next().unwrap_or_default().to_string()
}

fn millis(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// 读 `since_ms` 之后（None = 全部）的请求日志，按时间从早到晚。读坏的行跳过。
pub fn read_logs(dir: &Path, since_ms: Option<u64>) -> Vec<RequestLog> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    // 按文件名的日期先筛一遍（多留一天，时区边上的不丢）
    let since_day = since_ms.and_then(|ms| {
        chrono::DateTime::from_timestamp_millis(ms as i64)
            .map(|t| t.with_timezone(&Local).date_naive() - chrono::Duration::days(1))
    });
    let mut files: Vec<(NaiveDate, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == "jsonl").then_some(())?;
            let date = NaiveDate::parse_from_str(path.file_stem()?.to_str()?, "%Y-%m-%d").ok()?;
            since_day.is_none_or(|d| date >= d).then_some((date, path))
        })
        .collect();
    files.sort();
    let mut logs: Vec<RequestLog> = files
        .iter()
        .flat_map(|(_, path)| {
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<RequestLog>(line).ok())
                .collect::<Vec<_>>()
        })
        .filter(|log| since_ms.is_none_or(|ms| log.at_ms >= ms))
        .collect();
    logs.sort_by_key(|log| log.at_ms);
    logs
}

/// 删掉全部请求日志，返回删了几个文件。
pub fn clear_logs(dir: &Path) -> std::io::Result<usize> {
    let mut removed = 0;
    for entry in std::fs::read_dir(dir)?.flatten() {
        if entry.path().extension().is_some_and(|e| e == "jsonl") {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// 删掉超过保留期的日志；总量还超就从最旧的删（当天的不删）。
fn prune(dir: &Path, today: NaiveDate) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(NaiveDate, PathBuf, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let stem = path.file_stem()?.to_str()?.to_string();
            (path.extension()? == "jsonl").then_some(())?;
            let date = NaiveDate::parse_from_str(&stem, "%Y-%m-%d").ok()?;
            Some((date, path, e.metadata().map_or(0, |m| m.len())))
        })
        .collect();
    files.sort();
    let cutoff = today - chrono::Duration::days(KEEP_DAYS);
    files.retain(|(date, path, _)| {
        let old = *date < cutoff;
        if old {
            let _ = std::fs::remove_file(path);
        }
        !old
    });
    let mut total: u64 = files.iter().map(|(_, _, n)| n).sum();
    for (date, path, size) in &files {
        if total <= KEEP_BYTES || *date >= today {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total -= size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_forwarded_headers() {
        assert_eq!(host_of("https://user:pw@api.example.com:8443/v1"), "api.example.com:8443");
        assert_eq!(host_of("http://127.0.0.1:7999/v1"), "127.0.0.1:7999");
        assert!(
            forwarded("authorization") && forwarded("x-api-key") && forwarded("anthropic-version")
        );
        assert!(!forwarded("host") && !forwarded("content-length") && !forwarded(UPSTREAM));
    }

    #[test]
    fn parses_usage_from_both_protocols() {
        let t0 = Instant::now();
        let mut s = StreamStats::new(false);
        s.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"con", t0);
        assert_eq!(s.first_token_ms, None);
        s.feed(b"tent\":\"hi\"},\"finish_reason\":\"stop\"}]}\r\n\r\n", t0);
        s.feed(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":60}}}\n\ndata: [DONE]\n\n", t0);
        assert!(s.first_token_ms.is_some());
        assert_eq!((s.input, s.output, s.cache_read), (100, 7, 60));
        assert_eq!(s.finish_reason.as_deref(), Some("stop"));

        let mut a = StreamStats::new(true);
        a.feed(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"cache_read_input_tokens\":90}}}\n\n", t0);
        a.feed(b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n", t0);
        a.feed(b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":12}}\n\n", t0);
        assert_eq!((a.input, a.output, a.cache_read), (95, 12, 90));
        assert_eq!(a.finish_reason.as_deref(), Some("end_turn"));
        assert!(a.first_token_ms.is_some());
    }

    #[test]
    fn prunes_old_and_oversized_logs() {
        let dir = std::env::temp_dir().join(format!("cd-gateway-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
        for name in ["2026-09-01.jsonl", "2026-10-17.jsonl", "2026-10-18.jsonl", "notes.txt"] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        prune(&dir, today);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["2026-10-17.jsonl", "2026-10-18.jsonl", "notes.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_logs_back_since_a_time() {
        let dir = std::env::temp_dir().join(format!("cd-gateway-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let line = |at_ms: u64| {
            serde_json::to_string(&RequestLog {
                at_ms,
                purpose: "turn".into(),
                ..Default::default()
            })
            .unwrap()
        };
        let day = |ms: u64| {
            chrono::DateTime::from_timestamp_millis(ms as i64)
                .unwrap()
                .with_timezone(&Local)
                .format("%Y-%m-%d")
                .to_string()
        };
        let (old, new) = (1_700_000_000_000u64, 1_790_000_000_000u64);
        std::fs::write(dir.join(format!("{}.jsonl", day(old))), line(old) + "\n").unwrap();
        std::fs::write(
            dir.join(format!("{}.jsonl", day(new))),
            format!("{}\nnot json\n{}\n", line(new + 5), line(new)),
        )
        .unwrap();
        let all = read_logs(&dir, None);
        assert_eq!(all.iter().map(|l| l.at_ms).collect::<Vec<_>>(), [old, new, new + 5]);
        assert_eq!(read_logs(&dir, Some(new)).len(), 2);
        assert_eq!(clear_logs(&dir).unwrap(), 2);
        assert!(read_logs(&dir, None).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tags_nest_and_restore() {
        let gateway = Gateway::start(
            std::env::temp_dir().join(format!("cd-gateway-tag-{}", std::process::id())),
        )
        .unwrap();
        let session =
            |r: &Route| r.headers.iter().find(|(n, _)| *n == SESSION).map(|(_, v)| v.clone());
        assert_eq!(session(&route(Some(&gateway), "https://x/v1", "/chat/completions")), None);
        {
            let _outer = tag("s1", "turn", 3);
            {
                let _inner = tag("s1/t1", "subagent", 3);
                assert_eq!(
                    session(&route(Some(&gateway), "https://x/v1", "/a")).as_deref(),
                    Some("s1/t1")
                );
            }
            assert_eq!(
                session(&route(Some(&gateway), "https://x/v1", "/a")).as_deref(),
                Some("s1")
            );
        }
        assert_eq!(session(&route(Some(&gateway), "https://x/v1", "/a")), None);
        // 没开网关：直连、不带头
        let direct = route(None, "https://x/v1/", "/chat/completions");
        assert_eq!(direct.url, "https://x/v1/chat/completions");
        assert!(!direct.via_gateway());
    }
}
