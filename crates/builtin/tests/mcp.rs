//! MCP 连接层：本地 / Streamable HTTP / 旧版 SSE 三种传输，断线重连，授权失败。
//! 服务器是 `tools/mock-mcp-server.mjs`（需要 node）。

use claudesktop_builtin::mcp::McpHub;
use claudesktop_core::{McpServerView, McpState};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn script() -> String {
    // 不 canonicalize：Windows 上会加上 node 不认的长路径前缀
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/mock-mcp-server.mjs")
        .to_string_lossy()
        .into_owned()
}

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("claudesktop-mcp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 起一个远程模式的 mock 服务器，读出它监听的端口。释放时结束进程。
struct Remote(Child, u16);

impl Remote {
    fn start(mode: &str, token: &str) -> Self {
        let mut child = Command::new("node")
            .args([script().as_str(), mode, token])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("需要 node");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let port = line.trim().strip_prefix("PORT ").unwrap().parse().unwrap();
        Self(child, port)
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

/// 起一个 hub，状态上报都记下来。
struct Hub {
    hub: Arc<McpHub>,
    history: Arc<Mutex<Vec<Vec<McpServerView>>>>,
}

impl Hub {
    fn new(name: &str, servers: serde_json::Value) -> Self {
        let dir = workdir(name);
        let config = dir.join("mcp.json");
        std::fs::write(&config, serde_json::json!({ "mcpServers": servers }).to_string()).unwrap();
        let history = Arc::new(Mutex::new(Vec::new()));
        let sink = history.clone();
        let hub = McpHub::new(Some(config), dir, move |s| sink.lock().unwrap().push(s));
        hub.reload();
        Self { hub, history }
    }

    fn state(&self, name: &str) -> Option<McpState> {
        let history = self.history.lock().unwrap();
        history.last()?.iter().find(|s| s.name == name).map(|s| s.state.clone())
    }

    /// 等到某个服务的状态满足条件，返回那时的样子。
    fn wait(&self, what: &str, name: &str, ok: impl Fn(&McpServerView) -> bool) -> McpServerView {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let last = self.history.lock().unwrap().last().cloned().unwrap_or_default();
            if let Some(s) = last.iter().find(|s| s.name == name && ok(s)) {
                return s.clone();
            }
            assert!(Instant::now() < deadline, "等不到 {what}：{last:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn ready(&self, name: &str, tools: usize) {
        self.wait("ready", name, |s| s.state == McpState::Ready && s.tools == tools);
    }

    fn call(&self, server: &str, tool: &str, args: serde_json::Value) -> (bool, String) {
        let out = self.hub.call(server, tool, args, &|| false);
        (out.ok, out.content)
    }
}

/// 掉线 → 状态变成「重连中」→ 自动连回来，工具照常能调。
fn crash_and_recover(h: &Hub, name: &str) {
    h.call(name, "crash", serde_json::json!({}));
    // 远程服务要等下一个请求才发现会话没了
    let (ok, _) = h.call(name, "echo", serde_json::json!({ "text": "x" }));
    let _ = ok;
    let r = h.wait("reconnecting", name, |s| matches!(s.state, McpState::Reconnecting(_)));
    let McpState::Reconnecting(detail) = r.state else { unreachable!() };
    assert!(detail.contains("第 1 次重连"), "{detail}");
    h.ready(name, 3);
    assert_eq!(
        h.call(name, "echo", serde_json::json!({ "text": "again" })),
        (true, "echo: again".into())
    );
}

#[test]
fn stdio_servers_reconnect_after_the_process_dies() {
    let h = Hub::new(
        "stdio",
        serde_json::json!({ "local": { "command": "node", "args": [script()] } }),
    );
    h.ready("local", 3);
    assert_eq!(
        h.call("local", "echo", serde_json::json!({ "text": "hi" })),
        (true, "echo: hi".into())
    );
    crash_and_recover(&h, "local");
    h.hub.stop();
}

#[test]
fn streamable_http_servers_work_and_recover_from_lost_sessions() {
    let remote = Remote::start("--http", "tok123");
    // 请求头里的 ${VAR:-默认值} 换成默认值（没有这个环境变量）
    let h = Hub::new(
        "http",
        serde_json::json!({ "web": {
            "type": "http",
            "url": format!("http://127.0.0.1:{}/mcp", remote.1),
            "headers": { "Authorization": "Bearer ${CLAUDESKTOP_TEST_UNSET:-tok123}" },
        }}),
    );
    h.ready("web", 3);
    // tools/call 的响应是 SSE 流，工具变了的通知在响应前面：重新列出 4 个
    assert_eq!(
        h.call("web", "echo", serde_json::json!({ "text": "relist" })),
        (true, "echo: relist".into())
    );
    h.ready("web", 4);
    let (ok, content) = h.call("web", "boom", serde_json::json!({}));
    assert!(!ok && content.contains("boom failed"));
    // 服务器丢了会话：下一个请求 404，按断线重连（新会话里工具又是 3 个：mock 的 extra 不随会话走，
    // 这里只看连回来）
    h.call("web", "crash", serde_json::json!({}));
    let (ok, content) = h.call("web", "echo", serde_json::json!({ "text": "x" }));
    assert!(!ok && content.contains("会话过期"), "{content}");
    h.wait("reconnecting", "web", |s| matches!(s.state, McpState::Reconnecting(_)));
    h.wait("ready again", "web", |s| s.state == McpState::Ready);
    assert_eq!(
        h.call("web", "echo", serde_json::json!({ "text": "back" })),
        (true, "echo: back".into())
    );
    h.hub.stop();
}

#[test]
fn legacy_sse_servers_work_and_reconnect_when_the_stream_ends() {
    let remote = Remote::start("--sse", "");
    let h = Hub::new(
        "sse",
        serde_json::json!({ "old": { "type": "sse", "url": format!("http://127.0.0.1:{}/sse", remote.1) } }),
    );
    h.ready("old", 3);
    assert_eq!(
        h.call("old", "echo", serde_json::json!({ "text": "hi" })),
        (true, "echo: hi".into())
    );
    crash_and_recover(&h, "old");
    h.hub.stop();
}

#[test]
fn unauthorized_servers_fail_with_a_hint_and_can_be_retried() {
    let remote = Remote::start("--http", "right");
    let h = Hub::new(
        "auth",
        serde_json::json!({ "web": { "url": format!("http://127.0.0.1:{}/mcp", remote.1) } }),
    );
    let s = h.wait("failed", "web", |s| matches!(s.state, McpState::Failed(_)));
    let McpState::Failed(reason) = s.state else { unreachable!() };
    assert!(reason.contains("401") && reason.contains("headers"), "{reason}");
    // 手动重连：再走一遍握手（还是失败），状态先回到连接中
    let before = h.history.lock().unwrap().len();
    h.hub.reconnect("web");
    let history = h.history.lock().unwrap();
    assert!(history[before..].iter().any(|v| v.iter().any(|s| s.state == McpState::Connecting)));
    drop(history);
    h.wait("failed again", "web", |s| matches!(s.state, McpState::Failed(_)));
    assert!(matches!(h.state("web"), Some(McpState::Failed(_))));
    h.hub.stop();
}
