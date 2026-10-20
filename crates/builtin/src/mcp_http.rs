//! MCP 的远程传输：Streamable HTTP（`"type": "http"`，现行规范）和旧版 HTTP + SSE（`"type": "sse"`）。
//!
//! - **Streamable HTTP**：每条消息 POST 到同一个地址，`Accept` 同时给 JSON 和 SSE；服务器回 JSON
//!   （一条或一批）或一段 SSE 流（流里可能先有通知、最后是响应），通知回 202。`initialize` 的响应头
//!   `Mcp-Session-Id` 之后每个请求都带上；带着它回 404 说明会话过期了，按断线处理（外面会重连）。
//!   服务器主动推送用的 GET 流不开：只用到请求 / 响应，工具变了的通知也常随响应流一起来。
//! - **旧版 SSE**：先 GET 一条事件流，服务器在 `endpoint` 事件里给出 POST 地址；之后 POST 只管送达，
//!   响应和通知都从事件流里来，流断了就是断线。
//!
//! 收到的消息、发不出去的请求、断线都交给 [`Sink`]（`mcp.rs` 的 `Client`），JSON-RPC 的配对在那边做。

use crate::mcp_oauth::Auth;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Duration;

/// 旧版 SSE 服务器开流后多久内要给出 POST 地址
const ENDPOINT_TIMEOUT: Duration = Duration::from_secs(30);

/// 传输层收到的东西交给谁。
pub(crate) trait Sink: Send + Sync + 'static {
    /// 服务器发来的一条 JSON-RPC 消息
    fn message(&self, msg: Value);
    /// 请求 `id` 没送到 / 服务器回了错误状态
    fn fail(&self, id: u64, reason: String);
    /// 连接断了（事件流结束、会话过期）
    fn closed(&self, reason: String);
}

/// 连接失败的原因，分成两种：一种该走 OAuth，一种只能报给用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpenError {
    /// 服务器回了 401 且给了 Bearer 挑战：先授权再连
    NeedsAuth(String),
    /// 别的原因（连不上、404、配置错……）
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Streamable,
    Legacy,
}

pub(crate) struct Http {
    kind: Kind,
    agent: ureq::Agent,
    url: String,
    headers: BTreeMap<String, String>,
    /// OAuth 拿到的令牌（`headers` 里有 Authorization 时为 None，那时一直用固定头）
    auth: Option<Arc<Auth>>,
    /// Streamable：服务器给的会话 ID（POST 线程里写，所以包一层 Arc）
    session: Arc<Mutex<Option<String>>>,
    /// 最近一次 401 的 `WWW-Authenticate`：`establish` 据此决定要不要走 OAuth
    challenge: Arc<Mutex<Option<String>>>,
    /// 旧版：服务器给的 POST 地址
    endpoint: Option<String>,
}

impl Http {
    /// 建连接。旧版 SSE 要先开事件流、等到 POST 地址才返回；Streamable 不用先连。
    pub(crate) fn open(
        kind: Kind,
        url: &str,
        headers: BTreeMap<String, String>,
        auth: Option<Arc<Auth>>,
        sink: Weak<dyn Sink>,
        challenge: Arc<Mutex<Option<String>>>,
    ) -> Result<Self, OpenError> {
        let agent = ureq::Agent::config_builder()
            // 非 2xx 也要读响应体拿错误信息
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .build()
            .new_agent();
        let mut http = Self {
            kind,
            agent,
            url: url.to_string(),
            headers,
            auth,
            session: Arc::new(Mutex::new(None)),
            challenge,
            endpoint: None,
        };
        if kind == Kind::Legacy {
            http.endpoint = Some(http.open_event_stream(sink)?);
        }
        Ok(http)
    }

    /// 旧版：GET 事件流，后台线程读它；返回 `endpoint` 事件给的 POST 地址。
    fn open_event_stream(&self, sink: Weak<dyn Sink>) -> Result<String, OpenError> {
        let mut request = self.agent.get(&self.url).header("Accept", "text/event-stream");
        for (k, v) in &self.headers {
            request = request.header(k.as_str(), v.as_str());
        }
        if let Some(auth) = &self.auth {
            request = request.header("Authorization", format!("Bearer {}", auth.access_token()));
        }
        let response = request.call().map_err(|e| OpenError::Failed(send_error(&self.url, &e)))?;
        let status = response.status().as_u16();
        if status == 401 {
            let challenge = response
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            *self.challenge.lock().unwrap() = Some(challenge.clone());
            return Err(OpenError::NeedsAuth(challenge));
        }
        if !(200..300).contains(&status) {
            let text = response.into_body().read_to_string().unwrap_or_default();
            return Err(OpenError::Failed(status_reason(status, &text)));
        }
        let reader = BufReader::new(response.into_body().into_reader());
        let (tx, rx) = mpsc::channel();
        let base = self.url.clone();
        let _ = std::thread::Builder::new().name("mcp-sse".into()).spawn(move || {
            let mut tx = Some(tx);
            let result = read_sse(reader, |event, data| {
                // 只拿弱引用：连接释放后，下一条事件（服务器的保活）到来时线程就退出
                let Some(sink) = sink.upgrade() else { return false };
                if event == "endpoint" {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(resolve(&base, data.trim()));
                    }
                } else if let Ok(msg) = serde_json::from_str(data) {
                    sink.message(msg);
                }
                true
            });
            if let Some(sink) = sink.upgrade() {
                sink.closed(match result {
                    Ok(()) => "事件流断了".into(),
                    Err(e) => format!("事件流断了：{e}"),
                });
            }
        });
        rx.recv_timeout(ENDPOINT_TIMEOUT)
            .map_err(|_| OpenError::Failed("服务器没给出 POST 地址（endpoint 事件）".to_string()))
    }

    /// 发一条消息。在后台线程里 POST（响应可能是一段很长的流），结果经 `sink` 回来。
    pub(crate) fn send(&self, msg: &Value, sink: Arc<dyn Sink>) {
        let id = msg.get("id").and_then(Value::as_u64);
        let method = msg.get("method").and_then(Value::as_str);
        // 回给服务器的响应、通知：服务器回 202，不用读
        let expects_reply = id.is_some() && method.is_some();
        let url = match self.kind {
            Kind::Streamable => self.url.clone(),
            Kind::Legacy => self.endpoint.clone().unwrap_or_default(),
        };
        let had_session = self.session.lock().unwrap().clone();
        let agent = self.agent.clone();
        let headers = self.headers.clone();
        let body = msg.to_string();
        let kind = self.kind;
        let method = method.map(str::to_string);
        let session = self.session.clone();
        let auth = self.auth.clone();
        let challenge = self.challenge.clone();
        // 刷新失败时要按它忘掉令牌，所以取服务器地址（不是 POST 地址）
        let resource = self.url.clone();
        let _ = std::thread::Builder::new().name("mcp-post".into()).spawn(move || {
            let fail = |reason: String| {
                if let Some(id) = id.filter(|_| expects_reply) {
                    sink.fail(id, reason)
                }
            };
            // 重发时要重新取一次令牌（刚刷新过），所以请求现拼
            let build = || {
                let mut request = agent
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("Accept", "application/json, text/event-stream");
                for (k, v) in &headers {
                    request = request.header(k.as_str(), v.as_str());
                }
                if let Some(s) = &had_session {
                    request = request.header("Mcp-Session-Id", s.as_str());
                }
                if method.as_deref() != Some("initialize") {
                    request = request.header("MCP-Protocol-Version", crate::mcp::PROTOCOL_VERSION);
                }
                match &auth {
                    Some(auth) => {
                        request.header("Authorization", format!("Bearer {}", auth.access_token()))
                    }
                    None => request,
                }
            };
            let mut response = match build().send(&body) {
                Ok(r) => r,
                Err(e) => return fail(send_error(&url, &e)),
            };
            // 401：先把挑战记下来（上层据此决定走不走 OAuth），再考虑刷新
            if response.status().as_u16() == 401
                && let Some(value) =
                    response.headers().get("www-authenticate").and_then(|v| v.to_str().ok())
            {
                *challenge.lock().unwrap() = Some(value.to_string());
            }
            // 令牌过期：换一次再重发（每个请求最多一次，不放大）
            if response.status().as_u16() == 401
                && let Some(auth) = &auth
            {
                if auth.refresh().is_err() {
                    crate::mcp_oauth::forget(&resource);
                    let reason = "登录状态过期了，需要重新登录".to_string();
                    // 还没握手完（没有会话 id）：交给 establish 重新走授权，别当断线
                    return if had_session.is_some() { sink.closed(reason) } else { fail(reason) };
                }
                match build().send(&body) {
                    Ok(r) => response = r,
                    Err(e) => return fail(send_error(&url, &e)),
                }
            }
            let status = response.status().as_u16();
            if status == 404 && had_session.is_some() && kind == Kind::Streamable {
                return sink.closed("服务器那边的会话过期了".into());
            }
            if !(200..300).contains(&status) {
                let text = response.into_body().read_to_string().unwrap_or_default();
                return fail(status_reason(status, &text));
            }
            // 会话 ID 要在响应交出去之前记下：握手一完紧接着的请求就要带
            if let Some(s) = response.headers().get("mcp-session-id").and_then(|v| v.to_str().ok())
            {
                *session.lock().unwrap() = Some(s.to_string());
            }
            // 旧版的响应从事件流来
            if kind == Kind::Legacy || status == 202 || !expects_reply {
                return;
            }
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let mut body = response.into_body();
            if content_type.starts_with("text/event-stream") {
                let reader = BufReader::new(body.into_reader());
                let result = read_sse(reader, |_, data| {
                    if let Ok(msg) = serde_json::from_str(data) {
                        sink.message(msg);
                    }
                    true
                });
                if let Err(e) = result {
                    fail(format!("响应流断了：{e}"));
                }
                return;
            }
            let text = body.read_to_string().unwrap_or_default();
            match serde_json::from_str::<Value>(&text) {
                Ok(Value::Array(batch)) => batch.into_iter().for_each(|m| sink.message(m)),
                Ok(msg) => sink.message(msg),
                Err(_) if text.trim().is_empty() => fail("服务器回了空响应".into()),
                Err(e) => fail(format!("响应不是 JSON：{e}")),
            }
        });
    }
}

impl Drop for Http {
    /// Streamable 有会话时告诉服务器结束它（尽力而为，后台发，不等）。
    fn drop(&mut self) {
        let Some(session) = self.session.lock().unwrap().take() else {
            return;
        };
        let mut request = self.agent.delete(&self.url).header("Mcp-Session-Id", session.as_str());
        for (k, v) in &self.headers {
            request = request.header(k.as_str(), v.as_str());
        }
        let _ = std::thread::Builder::new().name("mcp-delete".into()).spawn(move || {
            let _ = request.call();
        });
    }
}

/// 请求没发出去 / 没回来的原因：连不上、找不到主机、超时说人话，其余附原文。
fn send_error(url: &str, e: &ureq::Error) -> String {
    match e {
        ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => {
            format!("连不上 {url}：服务没在运行，或者地址、端口不对")
        }
        ureq::Error::ConnectionFailed => format!("连不上 {url}：服务没在运行，或者地址、端口不对"),
        ureq::Error::HostNotFound => format!("找不到 {url} 的主机：检查地址拼写和网络"),
        ureq::Error::Timeout(_) => format!("请求 {url} 超时了：服务没有响应"),
        _ => format!("请求 {url} 失败：{e}"),
    }
}

/// 非 2xx 的原因。
/// 401 先说去设置页点「登录」（走 OAuth 的托管服务都是这样），手填 headers 退为兜底：
/// 服务器没给 Bearer 挑战时才走到这里（见 `mcp.rs` 的 `establish`），那种服务只能配凭据。
/// 403 是凭据过了但没这个权限，点登录再多次也没用，直说。
fn status_reason(status: u16, body: &str) -> String {
    let detail = crate::provider::error_message(body);
    match status {
        401 => format!(
            "HTTP 401：服务器要求授权。走 OAuth 的服务，在设置页「MCP」里点这个的「登录」；\
             不走 OAuth 的，就在 mcp.json 这个服务的 \"headers\" 里配上凭据（如 Authorization）。{detail}"
        ),
        403 => format!("HTTP 403：服务器拒绝了这次请求（凭据可能没有这个权限）。{detail}"),
        _ if detail.is_empty() => format!("HTTP {status}"),
        _ => format!("HTTP {status}：{detail}"),
    }
}

/// 旧版 `endpoint` 事件给的地址相对事件流地址解析：完整网址原样，`/` 开头接到主机后面，其余接到同一目录。
fn resolve(base: &str, endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return endpoint.to_string();
    }
    // scheme://host[:port]
    let origin_end = base
        .find("://")
        .map_or(0, |i| i + 3 + base[i + 3..].find('/').unwrap_or(base.len() - i - 3));
    if endpoint.starts_with('/') {
        return format!("{}{endpoint}", &base[..origin_end]);
    }
    let path = base[origin_end..].split(['?', '#']).next().unwrap_or_default();
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let dir = if dir.is_empty() { "/" } else { dir };
    format!("{}{dir}{endpoint}", &base[..origin_end])
}

/// 读 SSE：每条事件交给 `on_event(事件名, 数据)`（没写事件名的是 `message`），回 false 就停。
fn read_sse(
    reader: impl BufRead,
    mut on_event: impl FnMut(&str, &str) -> bool,
) -> std::io::Result<()> {
    let mut event = String::new();
    let mut data: Option<String> = None;
    for line in reader.lines() {
        let line = line?;
        let line = line.strip_suffix('\r').unwrap_or(&line);
        if line.is_empty() {
            if let Some(payload) = data.take() {
                let name = if event.is_empty() { "message" } else { event.as_str() };
                if !on_event(name, &payload) {
                    return Ok(());
                }
            }
            event.clear();
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => event = value.to_string(),
            "data" => match &mut data {
                Some(buf) => {
                    buf.push('\n');
                    buf.push_str(value);
                }
                None => data = Some(value.to_string()),
            },
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_endpoints() {
        let base = "http://127.0.0.1:3000/mcp/sse?x=1";
        assert_eq!(resolve(base, "https://a.b/m"), "https://a.b/m");
        assert_eq!(
            resolve(base, "/messages?sessionId=7"),
            "http://127.0.0.1:3000/messages?sessionId=7"
        );
        assert_eq!(resolve(base, "messages?s=1"), "http://127.0.0.1:3000/mcp/messages?s=1");
        assert_eq!(resolve("http://h:1", "m"), "http://h:1/m");
    }

    #[test]
    fn reads_named_sse_events() {
        let text = ": ping\n\nevent: endpoint\ndata: /m?s=1\n\ndata: {\"a\":1}\ndata: x\n\n";
        let mut got = Vec::new();
        read_sse(text.as_bytes(), |e, d| {
            got.push((e.to_string(), d.to_string()));
            true
        })
        .unwrap();
        assert_eq!(
            got,
            [
                ("endpoint".to_string(), "/m?s=1".to_string()),
                ("message".to_string(), "{\"a\":1}\nx".to_string()),
            ]
        );
    }
}
