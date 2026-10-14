//! BuiltinKernel 端到端测试：测试内起一个本地 HTTP 服务器回放 SSE，不联网。
//!
//! 真实端点测试默认忽略：
//! `CLAUDESKTOP_BASE_URL=… CLAUDESKTOP_MODEL=… CLAUDESKTOP_API_KEY=… cargo test -p claudesktop-builtin -- --ignored`

use claudesktop_builtin::{BuiltinConfig, BuiltinKernel, ProviderConfig};
use claudesktop_core::{EventSink, Kernel, KernelEvent, RoutedEvent};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 一次 HTTP 响应的剧本。
enum Reply {
    /// 200 + SSE 行（每行之间停顿 `gap_ms`）
    Sse { lines: Vec<String>, gap_ms: u64 },
    /// 非 2xx + JSON 错误体
    Status(u16, &'static str),
}

/// 按顺序回放剧本的本地服务器；返回 (base_url, 收到的请求体)。
fn serve(script: Vec<Reply>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let bodies: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = bodies.clone();
    std::thread::spawn(move || {
        for reply in script {
            let Ok((stream, _)) = listener.accept() else { return };
            let body = read_request(&stream);
            seen.lock().unwrap().push(body);
            let _ = respond(stream, reply);
        }
    });
    (base, bodies)
}

fn read_request(stream: &TcpStream) -> String {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).unwrap();
    String::from_utf8(body).unwrap()
}

fn respond(mut stream: TcpStream, reply: Reply) -> std::io::Result<()> {
    match reply {
        Reply::Status(code, body) => write!(
            stream,
            "HTTP/1.1 {code} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
        Reply::Sse { lines, gap_ms } => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
            )?;
            for line in lines {
                write!(stream, "{line}\n\n")?;
                stream.flush()?;
                std::thread::sleep(Duration::from_millis(gap_ms));
            }
            Ok(())
        }
    }
}

fn delta(field: &str, text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"{field}\":{}}},\"finish_reason\":null}}]}}",
        serde_json::to_string(text).unwrap()
    )
}

fn finish(reason: &str) -> String {
    format!("data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"{reason}\"}}]}}")
}

const USAGE: &str = r#"data: {"choices":[],"usage":{"prompt_tokens":6,"completion_tokens":14,"completion_tokens_details":{"reasoning_tokens":8}}}"#;

fn happy_stream(answer: &str) -> Reply {
    Reply::Sse {
        lines: vec![
            delta("reasoning_content", "先想一下"),
            ": ping".into(),
            delta("content", answer),
            finish("stop"),
            USAGE.into(),
            "data: [DONE]".into(),
        ],
        gap_ms: 5,
    }
}

struct Harness {
    kernel: BuiltinKernel,
    events: Arc<Mutex<Vec<RoutedEvent>>>,
}

impl Harness {
    fn new(base_url: String) -> Self {
        Self::with_cwd(base_url, std::env::temp_dir())
    }

    fn with_cwd(base_url: String, cwd: PathBuf) -> Self {
        let events: Arc<Mutex<Vec<RoutedEvent>>> = Arc::default();
        let sink: EventSink = {
            let events = events.clone();
            Arc::new(move |e| events.lock().unwrap().push(e))
        };
        let kernel = BuiltinKernel::spawn(
            BuiltinConfig {
                provider: ProviderConfig {
                    base_url,
                    api_key: Some("test-key".into()),
                    model: "mock-model".into(),
                },
                cwd,
                rules_path: None,
            },
            sink,
        )
        .unwrap();
        Self { kernel, events }
    }

    fn start(&self) -> String {
        self.kernel.new_session(1, None).unwrap();
        self.events
            .lock()
            .unwrap()
            .iter()
            .find_map(|r| match &r.event {
                KernelEvent::Started { session_id, .. } => Some(session_id.clone()),
                _ => None,
            })
            .expect("started")
    }

    fn wait(&self, what: &str, pred: impl Fn(&KernelEvent) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.events.lock().unwrap().iter().any(|r| pred(&r.event)) {
            assert!(Instant::now() < deadline, "timeout waiting for {what}: {:?}", self.events.lock().unwrap());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn count(&self, pred: impl Fn(&KernelEvent) -> bool) -> usize {
        self.events.lock().unwrap().iter().filter(|r| pred(&r.event)).count()
    }

    fn text(&self, thought: bool) -> String {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|r| match &r.event {
                KernelEvent::AgentMessageChunk { text } if !thought => Some(text.clone()),
                KernelEvent::AgentThoughtChunk { text } if thought => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn streams_reasoning_text_usage_and_ends_turn() {
    let (base, bodies) = serve(vec![happy_stream("你好！")]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, "打个招呼".into()).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));

    assert_eq!(h.text(true), "先想一下");
    assert_eq!(h.text(false), "你好！");
    assert_eq!(
        h.count(|e| matches!(
            e,
            KernelEvent::UsageUpdated { input_tokens: 6, output_tokens: 14, reasoning_tokens: 8 }
        )),
        1
    );
    assert_eq!(
        h.count(|e| matches!(e, KernelEvent::TurnEnded { stop_reason } if stop_reason == "end_turn")),
        1
    );
    // 所有会话事件都带 sessionId
    assert!(h.events.lock().unwrap().iter().all(|r| match r.event {
        KernelEvent::AgentMessageChunk { .. } | KernelEvent::TurnEnded { .. } => r.session.as_deref() == Some(sid.as_str()),
        _ => true,
    }));
    let body: serde_json::Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
    assert_eq!(body["model"], "mock-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "打个招呼");
}

#[test]
fn history_carries_into_next_turn() {
    let (base, bodies) = serve(vec![happy_stream("第一答"), happy_stream("第二答")]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, "第一问".into()).unwrap();
    h.wait("turn 1", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    h.kernel.send_prompt(&sid, "第二问".into()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.count(|e| matches!(e, KernelEvent::TurnEnded { .. })) < 2 {
        assert!(Instant::now() < deadline, "turn 2 timeout");
        std::thread::sleep(Duration::from_millis(10));
    }
    let body: serde_json::Value = serde_json::from_str(&bodies.lock().unwrap()[1]).unwrap();
    let roles: Vec<_> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| format!("{}:{}", m["role"].as_str().unwrap(), m["content"].as_str().unwrap().chars().take(3).collect::<String>()))
        .collect();
    assert_eq!(roles[1..], ["user:第一问", "assistant:第一答", "user:第二问"]);
}

#[test]
fn retries_transient_errors_then_succeeds() {
    let (base, _) = serve(vec![
        Reply::Status(429, r#"{"error":{"message":"Resource has been exhausted"}}"#),
        happy_stream("重试后成功"),
    ]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, "x".into()).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(h.text(false), "重试后成功");
    assert_eq!(
        h.count(|e| matches!(e, KernelEvent::Status(s) if s.contains("HTTP 429") && s.contains("重试"))),
        1
    );
}

#[test]
fn non_retryable_error_fails_the_turn() {
    let (base, _) = serve(vec![Reply::Status(401, r#"{"error":{"message":"invalid api key"}}"#)]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, "x".into()).unwrap();
    h.wait("prompt failed", |e| {
        matches!(e, KernelEvent::PromptFailed { message } if message.contains("401") && message.contains("invalid api key"))
    });
    // 失败后会话仍可用：可以立刻发下一轮
    assert_eq!(h.count(|e| matches!(e, KernelEvent::TurnEnded { .. })), 0);
}

#[test]
fn cancel_stops_events_immediately() {
    let lines = (0..200).map(|i| delta("content", &format!("{i} "))).collect();
    let (base, _) = serve(vec![Reply::Sse { lines, gap_ms: 20 }]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, "长回答".into()).unwrap();
    h.wait("some text", |e| matches!(e, KernelEvent::AgentMessageChunk { .. }));
    h.kernel.cancel(&sid).unwrap();
    let after_cancel = h.count(|_| true);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(h.count(|_| true), after_cancel, "no events after cancel");
    assert_eq!(
        h.count(|e| matches!(e, KernelEvent::TurnEnded { stop_reason } if stop_reason == "cancelled")),
        1
    );
    // 取消后立即可以开始新的一轮（不会报「上一轮还没结束」）
    assert!(h.kernel.send_prompt(&sid, "再来".into()).is_ok());
}

// ---------- 工具循环 ----------

/// 模型这一步只发一个工具调用
fn tool_call(id: &str, name: &str, args: serde_json::Value) -> Reply {
    let call = serde_json::json!({
        "choices": [{ "index": 0, "delta": { "tool_calls": [{
            "index": 0, "id": id, "type": "function",
            "function": { "name": name, "arguments": args.to_string() }
        }]}, "finish_reason": null }]
    });
    Reply::Sse {
        lines: vec![
            delta("content", "我先看看。"),
            format!("data: {call}"),
            finish("tool_calls"),
            "data: [DONE]".into(),
        ],
        gap_ms: 5,
    }
}

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("claudesktop-kernel-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

impl Harness {
    fn wait_n(&self, what: &str, n: usize, pred: impl Fn(&KernelEvent) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.count(&pred) < n {
            assert!(Instant::now() < deadline, "timeout waiting for {what}: {:?}", self.events.lock().unwrap());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 第 n 个（从 1 起）权限请求：(请求 ID, 选项 ID 列表)
    fn permission(&self, n: usize) -> (u64, Vec<String>) {
        self.wait_n("permission request", n, |e| matches!(e, KernelEvent::PermissionRequest { .. }));
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|r| match &r.event {
                KernelEvent::PermissionRequest { id, options, .. } => {
                    Some((*id, options.iter().map(|o| o.id.clone()).collect()))
                }
                _ => None,
            })
            .nth(n - 1)
            .unwrap()
    }

    fn tool_status(&self, id: &str) -> Option<String> {
        self.events.lock().unwrap().iter().rev().find_map(|r| match &r.event {
            KernelEvent::ToolCallUpdated { id: i, status: Some(s), .. } if i == id => Some(s.clone()),
            _ => None,
        })
    }
}

fn body(bodies: &Arc<Mutex<Vec<String>>>, i: usize) -> serde_json::Value {
    serde_json::from_str(&bodies.lock().unwrap()[i]).unwrap()
}

#[test]
fn read_tool_runs_without_asking_and_feeds_result_back() {
    let dir = workdir("read");
    std::fs::write(dir.join("notes.txt"), "第一行\n秘密是 42\n").unwrap();
    let (base, bodies) = serve(vec![
        tool_call("call_1", "read_file", serde_json::json!({ "path": "notes.txt" })),
        happy_stream("秘密是 42。"),
    ]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, "秘密是什么".into()).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));

    assert_eq!(h.count(|e| matches!(e, KernelEvent::PermissionRequest { .. })), 0);
    assert_eq!(
        h.count(|e| matches!(e, KernelEvent::ToolCallStarted { title, kind, .. } if title == "Read notes.txt" && kind == "read")),
        1
    );
    assert_eq!(h.tool_status("call_1").as_deref(), Some("completed"));
    assert_eq!(h.text(false), "我先看看。秘密是 42。");

    // 第一步带工具定义；第二步历史里是 assistant(tool_calls) + tool 结果
    let first = body(&bodies, 0);
    let names: Vec<_> = first["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["read_file", "list_dir", "write_file", "edit_file", "shell"]);
    let second = body(&bodies, 1);
    let msgs = second["messages"].as_array().unwrap();
    let n = msgs.len();
    assert_eq!(msgs[n - 2]["role"], "assistant");
    assert_eq!(msgs[n - 2]["content"], "我先看看。");
    assert_eq!(msgs[n - 2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(msgs[n - 1]["role"], "tool");
    assert_eq!(msgs[n - 1]["tool_call_id"], "call_1");
    assert!(msgs[n - 1]["content"].as_str().unwrap().contains("2\t秘密是 42"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_asks_permission_and_allow_always_is_remembered() {
    let dir = workdir("write");
    let (base, bodies) = serve(vec![
        tool_call("w1", "write_file", serde_json::json!({ "path": "out/a.txt", "content": "A" })),
        happy_stream("写好了"),
        tool_call("w2", "write_file", serde_json::json!({ "path": "out/b.txt", "content": "B" })),
        happy_stream("又写好了"),
    ]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, "写 a".into()).unwrap();
    let (req, options) = h.permission(1);
    assert_eq!(options, ["allow_once", "allow_always", "reject_once"]);
    // 应答前不执行
    assert!(!dir.join("out/a.txt").exists());
    h.kernel.resolve_permission(req, Some("allow_always".into())).unwrap();
    h.wait("turn 1", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(std::fs::read_to_string(dir.join("out/a.txt")).unwrap(), "A");
    assert_eq!(h.tool_status("w1").as_deref(), Some("completed"));

    // 第二轮：规则已生效，不再询问
    h.kernel.send_prompt(&sid, "写 b".into()).unwrap();
    h.wait_n("turn 2", 2, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(h.count(|e| matches!(e, KernelEvent::PermissionRequest { .. })), 1);
    assert_eq!(std::fs::read_to_string(dir.join("out/b.txt")).unwrap(), "B");
    assert_eq!(bodies.lock().unwrap().len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rejected_call_is_reported_to_the_model() {
    let dir = workdir("reject");
    let (base, bodies) = serve(vec![
        tool_call("s1", "shell", serde_json::json!({ "command": "echo hi" })),
        happy_stream("好的，不执行。"),
    ]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, "跑个命令".into()).unwrap();
    let (req, options) = h.permission(1);
    assert_eq!(options, ["allow_once", "allow_always", "reject_once"]);
    h.kernel.resolve_permission(req, Some("reject_once".into())).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(h.tool_status("s1").as_deref(), Some("failed"));
    let second = body(&bodies, 1);
    let last = second["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["role"], "tool");
    assert!(last["content"].as_str().unwrap().contains("denied"));
    // 已应答的请求不能再应答
    assert!(h.kernel.resolve_permission(req, None).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plan_mode_offers_read_only_tools_and_denies_writes() {
    let dir = workdir("plan");
    let (base, bodies) = serve(vec![
        tool_call("p1", "write_file", serde_json::json!({ "path": "x.txt", "content": "x" })),
        happy_stream("计划如下"),
    ]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.set_mode(&sid, "plan").unwrap();
    assert!(h.kernel.set_mode(&sid, "nope").is_err());
    h.kernel.send_prompt(&sid, "改一下".into()).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(h.count(|e| matches!(e, KernelEvent::PermissionRequest { .. })), 0);
    assert_eq!(h.tool_status("p1").as_deref(), Some("failed"));
    assert!(!dir.join("x.txt").exists());
    let first = body(&bodies, 0);
    let names: Vec<_> = first["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["read_file", "list_dir"]);
    assert!(first["messages"][0]["content"].as_str().unwrap().contains("Plan mode"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_while_waiting_for_permission() {
    let dir = workdir("cancel-perm");
    let (base, _) = serve(vec![tool_call("c1", "write_file", serde_json::json!({ "path": "x.txt", "content": "x" }))]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, "写".into()).unwrap();
    let (req, _) = h.permission(1);
    h.kernel.cancel(&sid).unwrap();
    let after_cancel = h.count(|_| true);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(h.count(|_| true), after_cancel, "no events after cancel");
    assert!(h.kernel.resolve_permission(req, Some("allow_once".into())).is_err());
    assert!(!dir.join("x.txt").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_arguments_fail_the_call_not_the_turn() {
    let dir = workdir("badargs");
    let (base, bodies) = serve(vec![
        tool_call("b1", "read_file", serde_json::json!({ "file": "x" })),
        happy_stream("参数错了"),
    ]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, "读".into()).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { stop_reason } if stop_reason == "end_turn"));
    assert_eq!(h.tool_status("b1").as_deref(), Some("failed"));
    let second = body(&bodies, 1);
    let last = second["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(last["content"].as_str().unwrap().contains("path"), "{last}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 真实端点（如 Antigravity Tools 的 gemini-3.8-flash）。
#[test]
#[ignore]
fn live_endpoint_streams_a_reply() {
    let config = BuiltinConfig::from_env().expect("需要 CLAUDESKTOP_BASE_URL / CLAUDESKTOP_MODEL");
    let events: Arc<Mutex<Vec<RoutedEvent>>> = Arc::default();
    let sink: EventSink = {
        let events = events.clone();
        Arc::new(move |e| events.lock().unwrap().push(e))
    };
    let h = Harness {
        kernel: BuiltinKernel::spawn(config, sink).unwrap(),
        events,
    };
    let sid = h.start();
    h.kernel
        .send_prompt(&sid, "用一句话介绍你自己，不超过 30 个字。".into())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if h.count(|e| matches!(e, KernelEvent::TurnEnded { .. } | KernelEvent::PromptFailed { .. })) > 0 {
            break;
        }
        assert!(Instant::now() < deadline, "live turn timeout");
        std::thread::sleep(Duration::from_millis(50));
    }
    for r in h.events.lock().unwrap().iter() {
        match &r.event {
            KernelEvent::AgentMessageChunk { .. } | KernelEvent::AgentThoughtChunk { .. } => {}
            other => println!("[event] {other:?}"),
        }
    }
    println!("[thought] {}", h.text(true));
    println!("[reply] {}", h.text(false));
    assert_eq!(h.count(|e| matches!(e, KernelEvent::PromptFailed { .. })), 0);
    assert!(!h.text(false).trim().is_empty());
}

/// 真实端点上的工具循环：读文件 → 写文件（自动允许一次）→ 回答。
#[test]
#[ignore]
fn live_endpoint_tool_loop() {
    let mut config = BuiltinConfig::from_env().expect("需要 CLAUDESKTOP_BASE_URL / CLAUDESKTOP_MODEL");
    let dir = workdir("live");
    std::fs::write(dir.join("notes.txt"), "the secret number is 4217\n").unwrap();
    config.cwd = dir.clone();
    config.rules_path = None;
    let events: Arc<Mutex<Vec<RoutedEvent>>> = Arc::default();
    let sink: EventSink = {
        let events = events.clone();
        Arc::new(move |e| events.lock().unwrap().push(e))
    };
    let h = Harness {
        kernel: BuiltinKernel::spawn(config, sink).unwrap(),
        events,
    };
    let sid = h.start();
    h.kernel
        .send_prompt(
            &sid,
            "读一下 notes.txt，把里面的秘密数字写进 answer.txt（只写数字），然后用一句话告诉我你做了什么。".into(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut answered = 0;
    loop {
        // 有权限请求就允许一次
        let pending: Vec<u64> = h
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|r| match &r.event {
                KernelEvent::PermissionRequest { id, .. } => Some(*id),
                _ => None,
            })
            .skip(answered)
            .collect();
        for id in pending {
            h.kernel.resolve_permission(id, Some("allow_once".into())).unwrap();
            answered += 1;
        }
        if h.count(|e| matches!(e, KernelEvent::TurnEnded { .. } | KernelEvent::PromptFailed { .. })) > 0 {
            break;
        }
        assert!(Instant::now() < deadline, "live turn timeout");
        std::thread::sleep(Duration::from_millis(50));
    }
    for r in h.events.lock().unwrap().iter() {
        match &r.event {
            KernelEvent::AgentMessageChunk { .. } | KernelEvent::AgentThoughtChunk { .. } => {}
            other => println!("[event] {other:?}"),
        }
    }
    println!("[reply] {}", h.text(false));
    assert_eq!(h.count(|e| matches!(e, KernelEvent::PromptFailed { .. })), 0);
    assert_eq!(std::fs::read_to_string(dir.join("answer.txt")).unwrap().trim(), "4217");
    let _ = std::fs::remove_dir_all(&dir);
}
