//! 截断保护：`finish_reason == "length"` 的工具调用一律不执行（见 `run_steps` 里的闸）。
//!
//! 自己的小号 `serve` / `Reply` / `tool_call`（`tests/kernel.rs` 里那份别的 lane 也在改，
//! 不互相引用）。`finish("length")` 就是「回复被输出上限截断」的剧本。

use claudesktop_builtin::{BuiltinConfig, BuiltinKernel, ProviderConfig};
use claudesktop_core::{EventSink, Kernel, KernelEvent, RoutedEvent, SessionSpec, UserPrompt};
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
    stream.set_nodelay(true)?;
    match reply {
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

/// 一次工具调用；`finish` 那一行由调用方给（`tool_calls` 正常、`length` 被截断）。
fn tool_call_reply(id: &str, name: &str, args: serde_json::Value, reason: &str) -> Reply {
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
            finish(reason),
            r#"data: {"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":8}}"#.into(),
            "data: [DONE]".into(),
        ],
        gap_ms: 5,
    }
}

/// 参数看起来完整、但 `finish_reason` 是 `length` 的调用（最危险的那种）。
fn truncated_call(id: &str, name: &str, args: serde_json::Value) -> Reply {
    tool_call_reply(id, name, args, "length")
}

/// 普通的纯文字回复（`stop`）。
fn happy_stream(answer: &str) -> Reply {
    Reply::Sse {
        lines: vec![delta("content", answer), finish("stop"), "data: [DONE]".into()],
        gap_ms: 5,
    }
}

struct Harness {
    kernel: BuiltinKernel,
    events: Arc<Mutex<Vec<RoutedEvent>>>,
}

impl Harness {
    fn new(base_url: String, cwd: PathBuf) -> Self {
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
                    ..Default::default()
                },
                cwd,
                rules_path: None,
                user_instructions: None,
                context_windows: HashMap::new(),
                no_vision_models: Vec::new(),
                mcp_config: None,
                plugins: Vec::new(),
                max_steps: claudesktop_builtin::DEFAULT_MAX_STEPS,
                session_titles: false,
                memory_path: None,
                memory_suggest: false,
                auto_mode: None,
                chat_model: None,
                subagent_model: None,
                chat_web: true,
                chat_instructions: None,
            },
            sink,
        )
        .unwrap();
        Self { kernel, events }
    }

    fn start(&self) -> String {
        self.kernel.new_session(1, SessionSpec::default()).unwrap();
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
}

/// 每个 harness 一个空的工作目录（同一进程里的测试并行跑，按名字区分）。
fn workdir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("claudesktop-truncation-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 下一步请求里的最后一条消息（模型重发前看到的那条）。
fn last_message(body: &str) -> serde_json::Value {
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    body["messages"].as_array().unwrap().last().cloned().unwrap()
}

#[test]
fn truncated_tool_call_is_never_executed() {
    // 参数看着是完整的（路径和内容都有），但回复被输出上限截断了：不许写
    let dir = workdir("write");
    let (base, bodies) = serve(vec![
        truncated_call(
            "w1",
            "write_file",
            serde_json::json!({ "path": "a.txt", "content": "半截内容" }),
        ),
        happy_stream("好的，重发了。"),
    ]);
    let h = Harness::new(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("写个文件")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));

    // 1. 文件没被写
    assert!(!dir.join("a.txt").exists(), "被截断的 write_file 不该落盘");
    // 2. 界面看得见这一行（状态 failed），且没有 diff / 权限请求
    assert_eq!(
        h.count(|e| matches!(
            e,
            KernelEvent::ToolCallStarted { id, title, status, .. }
                if id == "w1" && title == "write_file" && status == "failed"
        )),
        1,
        "截断的调用要报一行 failed 的工具"
    );
    assert_eq!(
        h.count(|e| matches!(
            e,
            KernelEvent::ToolCallUpdated { id, status: Some(status), raw_output: Some(raw), .. }
                if id == "w1" && status == "failed" && raw == "回复被截断，未执行"
        )),
        1
    );
    assert_eq!(h.count(|e| matches!(e, KernelEvent::ToolCallDiff { .. })), 0, "不该发 diff");
    assert_eq!(
        h.count(|e| matches!(e, KernelEvent::PermissionRequest { .. })),
        0,
        "没执行就不用问权限"
    );

    // 3. 模型收到的 tool 结果是错误 + 要求重发；下一步请求里带着这条 tool 消息（历史没断）
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "截断后要再请求一次让模型重发");
    let second: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    let messages = second["messages"].as_array().unwrap();
    let tool = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .expect("下一步请求里必须有那条 tool 消息");
    assert_eq!(tool["tool_call_id"], "w1");
    let content = tool["content"].as_str().unwrap();
    assert!(content.contains("was not executed"), "要说清楚没执行：{content}");
    assert!(content.contains("output token limit"), "要说清楚原因：{content}");
    assert!(content.contains("Re-issue"), "要要求重发：{content}");
    let assistant = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "assistant" && m["tool_calls"].is_array())
        .expect("assistant 消息也要进历史");
    assert_eq!(assistant["tool_calls"][0]["id"], "w1");
    // 最后一条是工具结果（下一轮请求就靠它）
    assert_eq!(last_message(&bodies[1])["role"], "tool");
}

#[test]
fn truncated_edit_file_does_not_touch_other_lines() {
    // 截断的 old_string 可能「唯一匹配」到不相干的位置：一个字节都不许动
    let dir = workdir("edit");
    std::fs::write(dir.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let (base, _bodies) = serve(vec![
        truncated_call(
            "e1",
            "edit_file",
            serde_json::json!({ "path": "a.txt", "old_string": "beta", "new_string": "BETA" }),
        ),
        happy_stream("重发一次。"),
    ]);
    let h = Harness::new(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("改个文件")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "alpha\nbeta\ngamma\n");
}

#[test]
fn two_truncated_replies_in_a_row_stop_the_turn() {
    let dir = workdir("twice");
    let (base, bodies) = serve(vec![
        truncated_call(
            "w1",
            "write_file",
            serde_json::json!({ "path": "a.txt", "content": "一" }),
        ),
        truncated_call(
            "w2",
            "write_file",
            serde_json::json!({ "path": "a.txt", "content": "二" }),
        ),
        // 剧本只给两条：真要是还请求第三次，测试会因为没回复而失败
    ]);
    let h = Harness::new(base, dir.clone());
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("写文件")).unwrap();
    h.wait("prompt failed", |e| matches!(e, KernelEvent::PromptFailed { .. }));

    let message = h
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|r| match &r.event {
            KernelEvent::PromptFailed { message } => Some(message.clone()),
            _ => None,
        })
        .unwrap();
    assert!(message.contains("连续 2 次回复被输出上限截断"), "原因要说清楚：{message}");
    assert!(!dir.join("a.txt").exists(), "两次都不许落盘");
    assert_eq!(bodies.lock().unwrap().len(), 2, "不该再请求第三次");
    // 没正常结束这一轮
    assert_eq!(h.count(|e| matches!(e, KernelEvent::TurnEnded { .. })), 0);
}

#[test]
fn plain_length_reply_ends_the_turn_normally() {
    // 没有工具调用时 `length` 就是「回复太长」：照常结束这一轮，不加闸
    let dir = workdir("plain");
    let (base, _bodies) = serve(vec![Reply::Sse {
        lines: vec![delta("content", "很长的回答"), finish("length"), "data: [DONE]".into()],
        gap_ms: 5,
    }]);
    let h = Harness::new(base, dir);
    let sid = h.start();
    h.kernel.send_prompt(&sid, UserPrompt::text("讲讲")).unwrap();
    h.wait("turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    assert_eq!(
        h.count(|e| matches!(
            e,
            KernelEvent::TurnEnded { stop_reason } if stop_reason == "max_tokens"
        )),
        1
    );
    assert_eq!(h.count(|e| matches!(e, KernelEvent::ToolCallStarted { .. })), 0);
    assert_eq!(h.count(|e| matches!(e, KernelEvent::PromptFailed { .. })), 0);
}
