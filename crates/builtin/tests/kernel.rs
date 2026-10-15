//! BuiltinKernel 端到端测试：测试内起一个本地 HTTP 服务器回放 SSE，不联网。
//!
//! 真实端点测试默认忽略：
//! `CLAUDESKTOP_BASE_URL=… CLAUDESKTOP_MODEL=… CLAUDESKTOP_API_KEY=… cargo test -p claudesktop-builtin -- --ignored`

use claudesktop_builtin::{BuiltinConfig, BuiltinKernel, Protocol, ProviderConfig};
use claudesktop_core::{EventSink, Kernel, KernelEvent, RoutedEvent, UserPrompt};
use std::collections::HashMap;
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
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
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
        Self::with_config(
            ProviderConfig {
                base_url,
                api_key: Some("test-key".into()),
                model: "mock-model".into(),
                ..Default::default()
            },
            cwd,
        )
    }

    /// 指定协议的端点（Anthropic 原生等）。
    fn with_config(provider: ProviderConfig, cwd: PathBuf) -> Self {
        Self::with_windows(provider, cwd, HashMap::new())
    }

    /// 带上下文窗口（自动压缩用）。
    fn with_windows(
        provider: ProviderConfig,
        cwd: PathBuf,
        context_windows: HashMap<String, u64>,
    ) -> Self {
        let events: Arc<Mutex<Vec<RoutedEvent>>> = Arc::default();
        let sink: EventSink = {
            let events = events.clone();
            Arc::new(move |e| events.lock().unwrap().push(e))
        };
        let kernel = BuiltinKernel::spawn(
            BuiltinConfig {
                provider,
                cwd,
                rules_path: None,
                user_instructions: None,
                context_windows,
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
            assert!(
                Instant::now() < deadline,
                "timeout waiting for {what}: {:?}",
                self.events.lock().unwrap()
            );
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
    h.kernel.send_prompt(&sid, UserPrompt::text("打个招呼")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));

    assert_eq!(h.text(true), "先想一下");
    assert_eq!(h.text(false), "你好！");
    assert_eq!(
        h.count(|e| matches!(
            e,
            KernelEvent::UsageUpdated {
                input_tokens: 6,
                output_tokens: 14,
                reasoning_tokens: 8,
                context_tokens: 20,
                cache_read_tokens: 0
            }
        )),
        1
    );
    assert_eq!(
        h.count(
            |e| matches!(e, KernelEvent::TurnEnded { stop_reason } if stop_reason == "end_turn")
        ),
        1
    );
    // 所有会话事件都带 sessionId
    assert!(h.events.lock().unwrap().iter().all(|r| match r.event {
        KernelEvent::AgentMessageChunk { .. } | KernelEvent::TurnEnded { .. } =>
            r.session.as_deref() == Some(sid.as_str()),
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
    h.kernel.send_prompt(&sid, UserPrompt::text("第一问")).unwrap();
    h.wait("turn 1", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    h.kernel.send_prompt(&sid, UserPrompt::text("第二问")).unwrap();
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
        .map(|m| {
            format!(
                "{}:{}",
                m["role"].as_str().unwrap(),
                m["content"].as_str().unwrap().chars().take(3).collect::<String>()
            )
        })
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
    h.kernel.send_prompt(&sid, UserPrompt::text("x")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(h.text(false), "重试后成功");
    assert_eq!(
        h.count(|e| matches!(
            e,
            KernelEvent::Retrying { status: 429, attempt: 1, reason, .. } if reason.contains("exhausted")
        )),
        1
    );
}

#[test]
fn non_retryable_error_fails_the_turn() {
    let (base, _) = serve(vec![Reply::Status(401, r#"{"error":{"message":"invalid api key"}}"#)]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("x")).unwrap();
    h.wait("prompt failed", |e| {
        matches!(e, KernelEvent::PromptFailed { message } if message.contains("401") && message.contains("invalid api key"))
    });
    // 失败后会话仍可用：可以立刻发下一轮
    assert_eq!(h.count(|e| matches!(e, KernelEvent::TurnEnded { .. })), 0);
}

#[test]
fn retry_after_failure_keeps_roles_alternating() {
    let (base, bodies) = serve(vec![
        Reply::Status(400, r#"{"error":{"message":"bad request"}}"#),
        happy_stream("好了"),
    ]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("x")).unwrap();
    h.wait("prompt failed", |e| matches!(e, KernelEvent::PromptFailed { .. }));
    // 界面上的「重试」= 同一句再发一遍
    h.kernel.send_prompt(&sid, UserPrompt::text("x")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    let body: serde_json::Value = serde_json::from_str(&bodies.lock().unwrap()[1]).unwrap();
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["role"].as_str())
        .filter(|r| *r != "system")
        .collect();
    assert_eq!(roles, ["user", "assistant", "user"]);
}

#[test]
fn cancel_stops_events_immediately() {
    let lines = (0..200).map(|i| delta("content", &format!("{i} "))).collect();
    let (base, _) = serve(vec![Reply::Sse { lines, gap_ms: 20 }]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("长回答")).unwrap();
    h.wait("some text", |e| matches!(e, KernelEvent::AgentMessageChunk { .. }));
    h.kernel.cancel(&sid).unwrap();
    let after_cancel = h.count(|_| true);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(h.count(|_| true), after_cancel, "no events after cancel");
    assert_eq!(
        h.count(
            |e| matches!(e, KernelEvent::TurnEnded { stop_reason } if stop_reason == "cancelled")
        ),
        1
    );
    // 取消后立即可以开始新的一轮（不会报「上一轮还没结束」）
    assert!(h.kernel.send_prompt(&sid, UserPrompt::text("再来")).is_ok());
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
    let dir =
        std::env::temp_dir().join(format!("claudesktop-kernel-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

impl Harness {
    fn wait_n(&self, what: &str, n: usize, pred: impl Fn(&KernelEvent) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.count(&pred) < n {
            assert!(
                Instant::now() < deadline,
                "timeout waiting for {what}: {:?}",
                self.events.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 第 n 个（从 1 起）权限请求：(请求 ID, 选项 ID 列表)
    fn permission(&self, n: usize) -> (u64, Vec<String>) {
        self.wait_n("permission request", n, |e| {
            matches!(e, KernelEvent::PermissionRequest { .. })
        });
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
            KernelEvent::ToolCallUpdated { id: i, status: Some(s), .. } if i == id => {
                Some(s.clone())
            }
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
    h.kernel.send_prompt(&sid, UserPrompt::text("秘密是什么")).unwrap();
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
    let names: Vec<_> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "read_file",
            "list_dir",
            "glob",
            "todo_write",
            "grep",
            "web_fetch",
            "write_file",
            "edit_file",
            "shell"
        ]
    );
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

/// 模型这一步发多个工具调用
fn tool_calls(calls: &[(&str, &str, serde_json::Value)]) -> Reply {
    let calls: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(i, (id, name, args))| {
            serde_json::json!({
                "index": i, "id": id, "type": "function",
                "function": { "name": name, "arguments": args.to_string() }
            })
        })
        .collect();
    let chunk = serde_json::json!({
        "choices": [{ "index": 0, "delta": { "tool_calls": calls }, "finish_reason": null }]
    });
    Reply::Sse {
        lines: vec![format!("data: {chunk}"), finish("tool_calls"), "data: [DONE]".into()],
        gap_ms: 5,
    }
}

#[test]
fn consecutive_reads_run_together_and_results_keep_call_order() {
    let dir = workdir("parallel");
    std::fs::write(dir.join("a.txt"), "AAA\n").unwrap();
    std::fs::write(dir.join("b.txt"), "BBB\n").unwrap();
    let (base, bodies) = serve(vec![
        tool_calls(&[
            ("r1", "read_file", serde_json::json!({ "path": "a.txt" })),
            ("r2", "read_file", serde_json::json!({ "path": "b.txt" })),
            ("w", "write_file", serde_json::json!({ "path": "c.txt", "content": "C" })),
            ("r3", "read_file", serde_json::json!({ "path": "c.txt" })),
        ]),
        happy_stream("好了"),
    ]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("读读写写")).unwrap();
    let (req, _) = h.permission(1);
    h.kernel.resolve_permission(req, Some("allow_once".into())).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));

    // 事件顺序：r1、r2 两行先一起报出，再开始执行；写入要等它们都完成，r3 在写入之后才读
    let order: Vec<String> = h
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| match &r.event {
            KernelEvent::ToolCallStarted { id, .. } => Some(format!("start:{id}")),
            KernelEvent::ToolCallUpdated { id, status: Some(s), .. } if s != "pending" => {
                Some(format!("{s}:{id}"))
            }
            _ => None,
        })
        .collect();
    let pos = |s: &str| {
        order.iter().position(|o| o == s).unwrap_or_else(|| panic!("{s} missing: {order:?}"))
    };
    assert!(pos("start:r2") < pos("in_progress:r1"), "{order:?}");
    assert!(
        pos("completed:r1") < pos("start:w") && pos("completed:r2") < pos("start:w"),
        "{order:?}"
    );
    assert!(pos("completed:w") < pos("start:r3"), "{order:?}");
    assert_eq!(h.tool_status("r3").as_deref(), Some("completed"));

    let second = body(&bodies, 1);
    let results: Vec<(String, String)> = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| {
            (m["tool_call_id"].as_str().unwrap().into(), m["content"].as_str().unwrap().into())
        })
        .collect();
    let ids: Vec<_> = results.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["r1", "r2", "w", "r3"]);
    assert!(
        results[0].1.contains("AAA") && results[1].1.contains("BBB") && results[3].1.contains('C')
    );
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
    h.kernel.send_prompt(&sid, UserPrompt::text("写 a")).unwrap();
    let (req, options) = h.permission(1);
    assert_eq!(options, ["allow_once", "allow_always", "reject_once"]);
    // 应答前不执行
    assert!(!dir.join("out/a.txt").exists());
    h.kernel.resolve_permission(req, Some("allow_always".into())).unwrap();
    h.wait("turn 1", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(std::fs::read_to_string(dir.join("out/a.txt")).unwrap(), "A");
    assert_eq!(h.tool_status("w1").as_deref(), Some("completed"));

    // 第二轮：规则已生效，不再询问
    h.kernel.send_prompt(&sid, UserPrompt::text("写 b")).unwrap();
    h.wait_n("turn 2", 2, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(h.count(|e| matches!(e, KernelEvent::PermissionRequest { .. })), 1);
    assert_eq!(std::fs::read_to_string(dir.join("out/b.txt")).unwrap(), "B");
    assert_eq!(bodies.lock().unwrap().len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn set_model_switches_the_session_and_new_sessions() {
    let (base, bodies) = serve(vec![happy_stream("一"), happy_stream("二"), happy_stream("三")]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("a")).unwrap();
    h.wait("turn 1", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert!(h.kernel.set_model(&sid, "  ").is_err());
    assert!(h.kernel.set_model("nope", "m2").is_err());
    h.kernel.set_model(&sid, "m2").unwrap();
    h.kernel.send_prompt(&sid, UserPrompt::text("b")).unwrap();
    h.wait_n("turn 2", 2, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(body(&bodies, 0)["model"], "mock-model");
    assert_eq!(body(&bodies, 1)["model"], "m2");
    // 新会话沿用最近选的模型
    h.kernel.new_session(2, None).unwrap();
    h.wait_n("second session", 2, |e| matches!(e, KernelEvent::Started { .. }));
    let models: Vec<_> = h
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| match &r.event {
            KernelEvent::Started { model, .. } => model.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(models, ["mock-model", "m2"]);
}

#[test]
fn edit_previews_diff_before_asking() {
    let dir = workdir("diff");
    std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let (base, _bodies) = serve(vec![
        tool_call(
            "e1",
            "edit_file",
            serde_json::json!({ "path": "a.txt", "old_string": "two", "new_string": "2" }),
        ),
        happy_stream("不改了"),
    ]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("改一下")).unwrap();
    let (req, _) = h.permission(1);
    {
        let events = h.events.lock().unwrap();
        let pos = |pred: &dyn Fn(&KernelEvent) -> bool| events.iter().position(|r| pred(&r.event));
        let diff_at = pos(&|e| matches!(e, KernelEvent::ToolCallDiff { id, .. } if id == "e1"))
            .expect("diff");
        let ask_at = pos(&|e| matches!(e, KernelEvent::PermissionRequest { .. })).unwrap();
        assert!(diff_at < ask_at, "diff 应先于权限请求");
        let KernelEvent::ToolCallDiff { diff, .. } = &events[diff_at].event else { unreachable!() };
        assert_eq!(
            (diff.path.as_str(), diff.added, diff.removed, diff.created),
            ("a.txt", 1, 1, false)
        );
        let tags: String = diff.lines.iter().map(|l| l.tag).collect();
        assert_eq!(tags, " -+ ");
    }
    h.kernel.resolve_permission(req, Some("reject_once".into())).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    // 拒绝后文件不变
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\ntwo\nthree\n");
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
    h.kernel.send_prompt(&sid, UserPrompt::text("跑个命令")).unwrap();
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
    h.kernel.send_prompt(&sid, UserPrompt::text("改一下")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(h.count(|e| matches!(e, KernelEvent::PermissionRequest { .. })), 0);
    assert_eq!(h.tool_status("p1").as_deref(), Some("failed"));
    assert!(!dir.join("x.txt").exists());
    let first = body(&bodies, 0);
    let names: Vec<_> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["read_file", "list_dir", "glob", "todo_write", "grep", "web_fetch"]);
    assert!(first["messages"][0]["content"].as_str().unwrap().contains("Plan mode"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 本地网页服务器：每个请求都回同一页 HTML；返回网址前缀
fn serve_page(html: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            read_request(&stream);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                html.len()
            );
        }
    });
    base
}

#[test]
fn web_fetch_asks_per_host_even_in_plan_mode_and_converts_html() {
    let dir = workdir("fetch");
    let page = serve_page(
        "<html><head><title>文档</title></head><body><h2>安装</h2><p>运行 <code>cargo add x</code></p></body></html>",
    );
    let (base, bodies) = serve(vec![
        tool_call("f1", "web_fetch", serde_json::json!({ "url": format!("{page}/docs") })),
        happy_stream("看完了"),
        tool_call("f2", "web_fetch", serde_json::json!({ "url": format!("{page}/other") })),
        happy_stream("又看完了"),
    ]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.set_mode(&sid, "plan").unwrap();
    h.kernel.send_prompt(&sid, UserPrompt::text("看下文档")).unwrap();
    let (req, options) = h.permission(1);
    assert_eq!(options, ["allow_once", "allow_always", "reject_once"]);
    h.kernel.resolve_permission(req, Some("allow_always".into())).unwrap();
    h.wait("turn 1", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(h.tool_status("f1").as_deref(), Some("completed"));
    assert_eq!(
        h.count(|e| matches!(e, KernelEvent::ToolCallStarted { title, kind, .. } if title == &format!("Fetch {page}/docs") && kind == "fetch")),
        1
    );
    let second = body(&bodies, 1);
    let msgs = second["messages"].as_array().unwrap();
    let result = msgs[msgs.len() - 1]["content"].as_str().unwrap();
    assert!(
        result.contains("Title: 文档") && result.contains("## 安装\n\n运行 `cargo add x`"),
        "{result}"
    );

    // 同一主机已总是允许：不再询问
    h.kernel.send_prompt(&sid, UserPrompt::text("再看一页")).unwrap();
    h.wait_n("turn 2", 2, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(h.count(|e| matches!(e, KernelEvent::PermissionRequest { .. })), 1);
    assert_eq!(h.tool_status("f2").as_deref(), Some("completed"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_while_waiting_for_permission() {
    let dir = workdir("cancel-perm");
    let (base, _) = serve(vec![tool_call(
        "c1",
        "write_file",
        serde_json::json!({ "path": "x.txt", "content": "x" }),
    )]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("写")).unwrap();
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
    h.kernel.send_prompt(&sid, UserPrompt::text("读")).unwrap();
    h.wait(
        "turn end",
        |e| matches!(e, KernelEvent::TurnEnded { stop_reason } if stop_reason == "end_turn"),
    );
    assert_eq!(h.tool_status("b1").as_deref(), Some("failed"));
    let second = body(&bodies, 1);
    let last = second["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(last["content"].as_str().unwrap().contains("path"), "{last}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_instructions_go_into_the_system_prompt_and_are_reread() {
    let dir = workdir("instructions");
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join("AGENTS.md"), "代码注释用中文").unwrap();
    let (base, bodies) = serve(vec![happy_stream("好"), happy_stream("好")]);
    let h = Harness::with_cwd(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("一")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    let system = body(&bodies, 0)["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(system.contains("代码注释用中文") && system.contains("AGENTS.md"), "{system}");

    std::fs::write(dir.join("AGENTS.md"), "提交信息用英文").unwrap();
    h.kernel.send_prompt(&sid, UserPrompt::text("二")).unwrap();
    h.wait_n("turn end", 2, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    let system = body(&bodies, 1)["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(system.contains("提交信息用英文") && !system.contains("代码注释用中文"), "{system}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn todo_write_replaces_the_plan_without_a_tool_row() {
    let (base, bodies) = serve(vec![
        tool_call(
            "t1",
            "todo_write",
            serde_json::json!({ "todos": [
                { "content": "读代码", "status": "completed" },
                { "content": "改代码", "status": "in_progress" }
            ]}),
        ),
        happy_stream("在改了"),
    ]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("改一下")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(h.count(|e| matches!(e, KernelEvent::ToolCallStarted { .. })), 0);
    assert_eq!(
        h.count(|e| matches!(e, KernelEvent::PlanReplaced { entries }
            if entries.len() == 2 && entries[1].content == "改代码" && entries[1].status == "in_progress")),
        1
    );
    let second = body(&bodies, 1);
    let last = second["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["tool_call_id"], "t1");
    assert!(last["content"].as_str().unwrap().contains("[>] 改代码"), "{last}");
}

// ---------- 上下文压缩 ----------

/// 一段回复，带指定的用量
fn reply_with_usage(answer: &str, prompt_tokens: u64) -> Reply {
    Reply::Sse {
        lines: vec![
            delta("content", answer),
            finish("stop"),
            format!(
                r#"data: {{"choices":[],"usage":{{"prompt_tokens":{prompt_tokens},"completion_tokens":10}}}}"#
            ),
            "data: [DONE]".into(),
        ],
        gap_ms: 5,
    }
}

/// 请求里的 (系统提示词, 其余消息的文本)
fn conversation(bodies: &Arc<Mutex<Vec<String>>>, i: usize) -> (String, Vec<String>) {
    let b = body(bodies, i);
    let msgs = b["messages"].as_array().unwrap();
    let system = msgs[0]["content"].as_str().unwrap_or_default().to_string();
    let rest =
        msgs[1..].iter().map(|m| m["content"].as_str().unwrap_or_default().to_string()).collect();
    (system, rest)
}

fn provider(base: String) -> ProviderConfig {
    ProviderConfig {
        base_url: base,
        api_key: Some("test-key".into()),
        model: "mock-model".into(),
        ..Default::default()
    }
}

#[test]
fn compacts_before_the_request_when_context_is_nearly_full() {
    let (base, bodies) = serve(vec![
        reply_with_usage("第一轮的回答", 850),
        happy_stream("## 摘要\n用户先问了第一轮的问题"),
        happy_stream("第二轮的回答"),
    ]);
    let windows = HashMap::from([("mock-model".to_string(), 1_000)]);
    let h = Harness::with_windows(provider(base), std::env::temp_dir(), windows);
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("第一轮的问题")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    // 上一轮结束时 860 / 1000，过了 80%：发第二轮前先压缩
    h.kernel.send_prompt(&sid, UserPrompt::text("第二轮的问题")).unwrap();
    h.wait_n("turn end", 2, |e| matches!(e, KernelEvent::TurnEnded { .. }));

    // 总结请求不带工具，内容是第一轮的记录
    let summary_req = body(&bodies, 1);
    assert!(summary_req.get("tools").is_none());
    let (_, rest) = conversation(&bodies, 1);
    assert!(rest[0].contains("第一轮的问题") && rest[0].contains("第一轮的回答"), "{rest:?}");
    assert!(!rest[0].contains("第二轮的问题"), "本轮保留，不进摘要");

    // 正式请求：摘要在系统提示词里，历史只剩本轮
    let (system, rest) = conversation(&bodies, 2);
    assert!(system.contains("用户先问了第一轮的问题"), "{system}");
    assert_eq!(rest, ["第二轮的问题"]);
    assert_eq!(h.count(|e| matches!(e, KernelEvent::Compacting)), 1);
    assert_eq!(
        h.count(|e| matches!(e, KernelEvent::Compacted { summary, context_tokens } if summary.starts_with("## 摘要") && *context_tokens > 0)),
        1
    );
    assert_eq!(h.text(false), "第一轮的回答第二轮的回答");
}

#[test]
fn context_overflow_error_compacts_and_resends_once() {
    let (base, bodies) = serve(vec![
        happy_stream("第一轮的回答"),
        Reply::Status(
            400,
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens"}}"#,
        ),
        happy_stream("早先的摘要"),
        happy_stream("第二轮的回答"),
    ]);
    let h = Harness::new(base);
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("第一轮的问题")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    h.kernel.send_prompt(&sid, UserPrompt::text("第二轮的问题")).unwrap();
    h.wait_n("turn end", 2, |e| matches!(e, KernelEvent::TurnEnded { .. }));

    assert_eq!(h.count(|e| matches!(e, KernelEvent::PromptFailed { .. })), 0);
    assert_eq!(h.count(|e| matches!(e, KernelEvent::Compacted { .. })), 1);
    let (system, rest) = conversation(&bodies, 3);
    assert!(system.contains("早先的摘要"), "{system}");
    assert_eq!(rest, ["第二轮的问题"]);
    assert_eq!(h.text(false), "第一轮的回答第二轮的回答");
}

#[test]
fn manual_compact_summarizes_everything_and_ends_a_turn() {
    let (base, bodies) =
        serve(vec![happy_stream("第一轮的回答"), happy_stream("全部的摘要"), happy_stream("好")]);
    let h = Harness::new(base);
    let sid = h.start();
    assert!(h.kernel.compact(&sid).is_err(), "空会话没什么可压");
    h.kernel.send_prompt(&sid, UserPrompt::text("第一轮的问题")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    h.kernel.compact(&sid).unwrap();
    // 压缩期间不能发消息
    assert!(h.kernel.send_prompt(&sid, UserPrompt::text("插队")).is_err());
    h.wait_n("turn end", 2, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(
        h.count(|e| matches!(e, KernelEvent::Compacted { summary, .. } if summary == "全部的摘要")),
        1
    );

    h.kernel.send_prompt(&sid, UserPrompt::text("第二轮的问题")).unwrap();
    h.wait_n("turn end", 3, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    let (system, rest) = conversation(&bodies, 2);
    assert!(system.contains("全部的摘要"), "{system}");
    assert_eq!(rest, ["第二轮的问题"]);
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
    let h = Harness { kernel: BuiltinKernel::spawn(config, sink).unwrap(), events };
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("用一句话介绍你自己，不超过 30 个字。")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if h.count(|e| {
            matches!(e, KernelEvent::TurnEnded { .. } | KernelEvent::PromptFailed { .. })
        }) > 0
        {
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
    let mut config =
        BuiltinConfig::from_env().expect("需要 CLAUDESKTOP_BASE_URL / CLAUDESKTOP_MODEL");
    let dir = workdir("live");
    std::fs::write(dir.join("notes.txt"), "the secret number is 4217\n").unwrap();
    config.cwd = dir.clone();
    config.rules_path = None;
    let events: Arc<Mutex<Vec<RoutedEvent>>> = Arc::default();
    let sink: EventSink = {
        let events = events.clone();
        Arc::new(move |e| events.lock().unwrap().push(e))
    };
    let h = Harness { kernel: BuiltinKernel::spawn(config, sink).unwrap(), events };
    let sid = h.start();
    h.kernel
        .send_prompt(
            &sid,
            UserPrompt::text("读一下 notes.txt，把里面的秘密数字写进 answer.txt（只写数字），然后用一句话告诉我你做了什么。"),
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
        if h.count(|e| {
            matches!(e, KernelEvent::TurnEnded { .. } | KernelEvent::PromptFailed { .. })
        }) > 0
        {
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

#[test]
fn resume_rebuilds_context_from_log() {
    use claudesktop_core::Record;
    let (base, bodies) = serve(vec![happy_stream("记得，是 42。")]);
    let h = Harness::new(base);
    let log = vec![
        Record::User { text: "秘密是 42，记住".into(), images: Vec::new() },
        Record::Event {
            event: KernelEvent::AgentMessageChunk { text: "好的，记住了。".into() }
        },
        Record::Event { event: KernelEvent::TurnEnded { stop_reason: "end_turn".into() } },
    ];
    h.kernel.resume_session(1, "builtin-old-1", None, &log).unwrap();
    let sid = h
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|r| match &r.event {
            KernelEvent::Started { session_id, resumed: true, .. } => Some(session_id.clone()),
            _ => None,
        })
        .expect("resumed start");
    h.kernel.send_prompt(&sid, UserPrompt::text("秘密是多少")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    let first = body(&bodies, 0);
    let msgs: Vec<_> = first["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| format!("{}:{}", m["role"].as_str().unwrap(), m["content"].as_str().unwrap()))
        .collect();
    assert!(msgs[0].contains("restored from a saved log"));
    assert_eq!(msgs[1..], ["user:秘密是 42，记住", "assistant:好的，记住了。", "user:秘密是多少"]);

    // 空日志 = 普通新会话
    h.kernel.resume_session(2, "x", None, &[]).unwrap();
    assert_eq!(h.count(|e| matches!(e, KernelEvent::Started { token: 2, resumed: false, .. })), 1);
}

/// Anthropic 原生协议：思考 / 正文 / 用量 / stop_reason 映射 + 工具调用整圈。
#[test]
fn anthropic_protocol_streams_and_runs_tools() {
    fn acme(payload: &str) -> String {
        format!("data: {payload}")
    }
    // 第一轮：思考 + tool_use(read_file)；第二轮：正文回答
    let first = Reply::Sse {
        lines: vec![
            acme(r#"{"type":"message_start","message":{"usage":{"input_tokens":9}}}"#),
            acme(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            acme(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"想想"}}"#,
            ),
            acme(
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
            ),
            acme(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"notes"}}"#,
            ),
            acme(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":".txt\"}"}}"#,
            ),
            acme(
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":12}}"#,
            ),
            acme(r#"{"type":"message_stop"}"#),
        ],
        gap_ms: 5,
    };
    let second = Reply::Sse {
        lines: vec![
            acme(r#"{"type":"message_start","message":{"usage":{"input_tokens":30}}}"#),
            acme(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            acme(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"文件里是 77。"}}"#,
            ),
            acme(
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            ),
            acme(r#"{"type":"message_stop"}"#),
        ],
        gap_ms: 5,
    };
    let (base, bodies) = serve(vec![first, second]);
    let dir = workdir("anthropic");
    let work = dir.clone();
    std::fs::write(work.join("notes.txt"), "magic 77\n").unwrap();
    let h = Harness::with_config(
        ProviderConfig {
            protocol: Protocol::Anthropic,
            base_url: base,
            api_key: Some("test-key".into()),
            model: "claude-mock".into(),
            ..Default::default()
        },
        work,
    );
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("读 notes.txt")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));

    assert_eq!(h.text(true), "想想");
    assert_eq!(h.text(false), "文件里是 77。");
    assert_eq!(
        h.count(|e| matches!(
            e,
            KernelEvent::UsageUpdated {
                input_tokens: 39,
                output_tokens: 17,
                reasoning_tokens: 0,
                context_tokens: 35,
                cache_read_tokens: 0
            }
        )),
        1
    );
    assert_eq!(
        h.count(
            |e| matches!(e, KernelEvent::TurnEnded { stop_reason } if stop_reason == "end_turn")
        ),
        1
    );
    // 请求体：system 顶层抽出 + max_tokens 必填 + 工具结果合并进 user 轮
    let first = body(&bodies, 0);
    assert_eq!(first["model"], "claude-mock");
    assert_eq!(first["max_tokens"], 16384);
    assert!(first["system"][0]["text"].as_str().unwrap().contains("coding agent"));
    assert_eq!(first["messages"][0]["role"], "user");
    assert_eq!(first["messages"][0]["content"][0]["type"], "text");
    assert_eq!(first["messages"][0]["content"][0]["text"], "读 notes.txt");
    let second = body(&bodies, 1);
    let results = &second["messages"][2];
    assert_eq!(results["role"], "user");
    assert_eq!(results["content"][0]["type"], "tool_result");
    assert_eq!(results["content"][0]["tool_use_id"], "toolu_1");
    assert!(results["content"][0]["content"].as_str().unwrap().contains("magic 77"));
    // 工具结果轮后面跟着带 tool_use 的 assistant 轮
    assert_eq!(second["messages"][1]["content"][0]["type"], "tool_use");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 图片附件：OpenAI 兼容协议上线时带 data URL 的分段 content。
#[test]
fn image_prompt_reaches_wire() {
    let (base, bodies) = serve(vec![happy_stream("看到图了")]);
    let h = Harness::new(base);
    let sid = h.start();
    let prompt = claudesktop_core::UserPrompt {
        text: "看图".into(),
        images: vec![claudesktop_core::PromptImage {
            media_type: "image/png".into(),
            data: "QUJD".into(),
        }],
    };
    h.kernel.send_prompt(&sid, prompt).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    let first = body(&bodies, 0);
    let content = first["messages"][1]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "看图");
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,QUJD");
}

/// Anthropic 原生协议下，图片走 `image` / `source.base64` 分段。
#[test]
fn anthropic_image_prompt_reaches_wire() {
    fn acme(payload: &str) -> String {
        format!("data: {payload}")
    }
    let stream = Reply::Sse {
        lines: vec![
            acme(r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#),
            acme(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            acme(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"看到图了"}}"#,
            ),
            acme(
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#,
            ),
            acme(r#"{"type":"message_stop"}"#),
        ],
        gap_ms: 5,
    };
    let (base, bodies) = serve(vec![stream]);
    let dir =
        std::env::temp_dir().join(format!("claudesktop-anthropic-img-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let h = Harness::with_config(
        ProviderConfig {
            protocol: Protocol::Anthropic,
            base_url: format!("{base}/v1"),
            api_key: Some("test-key".into()),
            model: "claude-sonnet-4-5".into(),
            ..Default::default()
        },
        dir.clone(),
    );
    let sid = h.start();
    let prompt = claudesktop_core::UserPrompt {
        text: "看图".into(),
        images: vec![claudesktop_core::PromptImage {
            media_type: "image/png".into(),
            data: "QUJD".into(),
        }],
    };
    h.kernel.send_prompt(&sid, prompt).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    let first = body(&bodies, 0);
    let content = first["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "看图");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["type"], "base64");
    assert_eq!(content[1]["source"]["media_type"], "image/png");
    assert_eq!(content[1]["source"]["data"], "QUJD");
    // 原生协议把 max_tokens 放在顶层（必填）
    assert!(first["max_tokens"].as_u64().is_some());
    let _ = std::fs::remove_dir_all(&dir);
}
