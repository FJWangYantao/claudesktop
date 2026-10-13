//! AcpKernel ↔ mock agent 端到端测试。
//!
//! 覆盖：initialize 握手、session/new、session/prompt、
//! agent/thought/user 流式块、plan、tool_call(+update)、
//! request_permission 自动应答、fs/read_text_file、TurnEnded。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use claudesktop_core::{AcpKernel, AcpKernelConfig, EventSink, Kernel, KernelEvent};

fn mock_command() -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mock = manifest
        .join("../../tools/mock-acp-agent.mjs")
        .display()
        .to_string()
        .replace('\\', "/");
    format!("node \"{mock}\"")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize workspace root")
}

#[test]
fn mock_agent_full_event_chain() {
    let events: Arc<Mutex<Vec<KernelEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink: EventSink = {
        let events = events.clone();
        Arc::new(move |event| {
            println!("[event] {} {:?}", event.tag(), event);
            events.lock().unwrap().push(event);
        })
    };

    let kernel = AcpKernel::spawn(
        AcpKernelConfig {
            command: mock_command(),
            cwd: workspace_root(),
            debug_wire: true,
        },
        sink,
    )
    .expect("spawn kernel");

    // 等待 Started
    wait_for(&events, 10_000, |e| matches!(e, KernelEvent::Started { .. }));

    kernel.send_prompt("你好 mock".into()).expect("send prompt");

    // 等待 TurnEnded
    wait_for(&events, 20_000, |e| matches!(e, KernelEvent::TurnEnded { .. }));

    kernel.shutdown();

    // ---- 断言 -----------------------------------------------------------
    let events = events.lock().unwrap();
    let tags: Vec<&str> = events.iter().map(|e| e.tag()).collect();

    let started = events
        .iter()
        .find_map(|e| match e {
            KernelEvent::Started { agent_name, session_id } => Some((agent_name, session_id)),
            _ => None,
        })
        .expect("Started event");
    assert_eq!(started.0, "mock-agent");
    assert!(started.1.starts_with("mock-session-"), "session id: {}", started.1);

    let assistant_text: String = events
        .iter()
        .filter_map(|e| match e {
            KernelEvent::AgentMessageChunk { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        assistant_text.contains("我是 mock 内核") && assistant_text.contains("工具完成：授权=true"),
        "assistant text: {assistant_text}"
    );

    assert!(tags.contains(&"agent_thought_chunk"), "missing thought chunk");
    assert!(tags.contains(&"plan_replaced"), "missing plan");
    let plan = events
        .iter()
        .find_map(|e| match e {
            KernelEvent::PlanReplaced { entries } => Some(entries.clone()),
            _ => None,
        })
        .expect("plan event");
    assert_eq!(plan.len(), 3);
    assert_eq!(plan[0].status, "completed");
    assert_eq!(plan[1].status, "in_progress");

    assert!(tags.contains(&"tool_call_started"), "missing tool call");
    let tool_started = events
        .iter()
        .find_map(|e| match e {
            KernelEvent::ToolCallStarted { status, .. } => Some(status.clone()),
            _ => None,
        })
        .expect("tool started");
    assert_eq!(tool_started, "in_progress");
    let tool_updated = events
        .iter()
        .find_map(|e| match e {
            KernelEvent::ToolCallUpdated { status, raw_output, .. } => Some((status.clone(), raw_output.clone())),
            _ => None,
        })
        .expect("tool update");
    assert_eq!(tool_updated.0.as_deref(), Some("completed"));
    assert!(
        tool_updated.1.as_deref().unwrap_or_default().contains("granted"),
        "raw output: {:?}",
        tool_updated.1
    );

    assert!(tags.contains(&"permission_auto"), "missing permission auto-approve");
    let permission = events
        .iter()
        .find_map(|e| match e {
            KernelEvent::PermissionAuto { chosen, .. } => Some(chosen.clone()),
            _ => None,
        })
        .expect("permission event");
    assert_eq!(permission, "允许一次");

    assert!(tags.contains(&"fs_read"), "missing fs read");
    let fs_ok = events.iter().any(|e| matches!(e, KernelEvent::FsRead { ok: true, .. }));
    assert!(fs_ok, "fs read should succeed");

    let turn = events
        .iter()
        .find_map(|e| match e {
            KernelEvent::TurnEnded { stop_reason } => Some(stop_reason.clone()),
            _ => None,
        })
        .expect("turn ended");
    assert_eq!(turn, "end_turn");

    assert!(!tags.contains(&"fatal"), "unexpected fatal: {events:?}");
}

/// 轮询等待直到谓词命中或超时（毫秒）。
fn wait_for(events: &Arc<Mutex<Vec<KernelEvent>>>, timeout_ms: u64, pred: impl Fn(&KernelEvent) -> bool) {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        {
            let events = events.lock().unwrap();
            if events.iter().any(&pred) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timeout after {timeout_ms}ms waiting for event; got: {:?}",
            events.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
