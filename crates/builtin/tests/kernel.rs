//! BuiltinKernel 端到端测试：测试内起一个本地 HTTP 服务器回放 SSE，不联网。
//!
//! 真实端点测试默认忽略：
//! `CLAUDESKTOP_BASE_URL=… CLAUDESKTOP_MODEL=… CLAUDESKTOP_API_KEY=… cargo test -p claudesktop-builtin -- --ignored`

use claudesktop_builtin::{BuiltinConfig, BuiltinKernel, ProviderConfig};
use claudesktop_core::{EventSink, Kernel, KernelEvent, RoutedEvent};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
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
                cwd: std::env::temp_dir(),
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
