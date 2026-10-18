//! AcpKernel ↔ mock agent 端到端测试。
//!
//! 覆盖：initialize 握手、session/new、session/prompt、
//! agent/thought/user 流式块、plan、tool_call(+update)、
//! request_permission 自动应答、fs/read_text_file、TurnEnded。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use claudesktop_core::{
    AcpKernel, AcpKernelConfig, EventSink, Kernel, KernelEvent, RoutedEvent, UserPrompt,
};

fn mock_command() -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mock =
        manifest.join("../../tools/mock-acp-agent.mjs").display().to_string().replace('\\', "/");
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
        Arc::new(move |routed: RoutedEvent| {
            let event = routed.event;
            println!("[event] {} {:?}", event.tag(), event);
            events.lock().unwrap().push(event);
        })
    };

    let kernel = AcpKernel::spawn(
        AcpKernelConfig {
            command: mock_command(),
            cwd: workspace_root(),
            debug_wire: true,
            auto_approve: true,
        },
        sink,
    )
    .expect("spawn kernel");

    let sid = start_session(&kernel, &events);
    kernel.send_prompt(&sid, UserPrompt::text("你好 mock")).expect("send prompt");

    // 等待 TurnEnded
    wait_for(&events, 20_000, |e| matches!(e, KernelEvent::TurnEnded { .. }));

    kernel.shutdown();

    // ---- 断言 -----------------------------------------------------------
    let events = events.lock().unwrap();
    let tags: Vec<&str> = events.iter().map(|e| e.tag()).collect();

    let started = events
        .iter()
        .find_map(|e| match e {
            KernelEvent::Started { agent_name, session_id, .. } => Some((agent_name, session_id)),
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
            KernelEvent::ToolCallUpdated { status, raw_output, .. } => {
                Some((status.clone(), raw_output.clone()))
            }
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

/// 等连接就绪 → 创建一个会话 → 返回它的 sessionId。
fn start_session(kernel: &AcpKernel, events: &Arc<Mutex<Vec<KernelEvent>>>) -> String {
    wait_for(events, 10_000, |e| matches!(e, KernelEvent::Connected { .. }));
    kernel.new_session(1, None).expect("new session");
    wait_for(events, 10_000, |e| matches!(e, KernelEvent::Started { .. }));

    events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            KernelEvent::Started { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .expect("started")
}

/// 一条连接上两个会话并发：事件按 sessionId 归属，互不串线。
#[test]
fn two_sessions_are_isolated() {
    let events: Arc<Mutex<Vec<RoutedEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink: EventSink = {
        let events = events.clone();
        Arc::new(move |routed| events.lock().unwrap().push(routed))
    };
    let kernel = AcpKernel::spawn(
        AcpKernelConfig {
            command: mock_command(),
            cwd: workspace_root(),
            debug_wire: false,
            auto_approve: true,
        },
        sink,
    )
    .expect("spawn kernel");

    let poll = |what: &str, pred: &dyn Fn(&[RoutedEvent]) -> bool| {
        let deadline = Instant::now() + Duration::from_millis(20_000);
        loop {
            if pred(&events.lock().unwrap()) {
                return;
            }
            assert!(Instant::now() < deadline, "timeout waiting for {what}");
            std::thread::sleep(Duration::from_millis(25));
        }
    };

    poll("connected", &|evs| evs.iter().any(|r| matches!(r.event, KernelEvent::Connected { .. })));
    kernel.new_session(11, None).expect("new session 1");
    kernel.new_session(22, None).expect("new session 2");
    poll("two sessions", &|evs| {
        evs.iter().filter(|r| matches!(r.event, KernelEvent::Started { .. })).count() == 2
    });

    let session_of = |token: u64| -> String {
        events
            .lock()
            .unwrap()
            .iter()
            .find_map(|r| match &r.event {
                KernelEvent::Started { token: t, session_id, .. } if *t == token => {
                    Some(session_id.clone())
                }
                _ => None,
            })
            .expect("session for token")
    };
    let (s1, s2) = (session_of(11), session_of(22));
    assert_ne!(s1, s2);

    kernel.send_prompt(&s1, UserPrompt::text("甲甲甲")).unwrap();
    kernel.send_prompt(&s2, UserPrompt::text("乙乙乙")).unwrap();
    poll("both turns end", &|evs| {
        evs.iter().filter(|r| matches!(r.event, KernelEvent::TurnEnded { .. })).count() == 2
    });
    kernel.shutdown();

    let evs = events.lock().unwrap();
    let thought_of = |sid: &str| -> String {
        evs.iter()
            .filter(|r| r.session.as_deref() == Some(sid))
            .filter_map(|r| match &r.event {
                KernelEvent::AgentThoughtChunk { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    };
    let (t1, t2) = (thought_of(&s1), thought_of(&s2));
    assert!(t1.contains("甲甲甲") && !t1.contains("乙乙乙"), "s1 thought: {t1}");
    assert!(t2.contains("乙乙乙") && !t2.contains("甲甲甲"), "s2 thought: {t2}");
    for sid in [&s1, &s2] {
        let ended = evs
            .iter()
            .filter(|r| r.session.as_deref() == Some(sid.as_str()))
            .filter(|r| matches!(r.event, KernelEvent::TurnEnded { .. }))
            .count();
        assert_eq!(ended, 1, "each session ends exactly one turn");
    }
}

/// 交互审批：内核发出 PermissionRequest 并挂起，UI 应答后工具才继续。
fn run_interactive(choose: &str) -> Vec<KernelEvent> {
    let events: Arc<Mutex<Vec<KernelEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink: EventSink = {
        let events = events.clone();
        Arc::new(move |routed: RoutedEvent| events.lock().unwrap().push(routed.event))
    };
    let kernel = AcpKernel::spawn(
        AcpKernelConfig {
            command: mock_command(),
            cwd: workspace_root(),
            debug_wire: false,
            auto_approve: false,
        },
        sink,
    )
    .expect("spawn kernel");
    let sid = start_session(&kernel, &events);
    kernel.send_prompt(&sid, UserPrompt::text("审批测试")).expect("send prompt");

    wait_for(&events, 10_000, |e| matches!(e, KernelEvent::PermissionRequest { .. }));
    // 请求挂起期间：本轮尚未结束，也没有任何自动应答
    {
        let evs = events.lock().unwrap();
        assert!(!evs.iter().any(|e| matches!(e, KernelEvent::TurnEnded { .. })));
        assert!(!evs.iter().any(|e| matches!(e, KernelEvent::PermissionAuto { .. })));
    }
    let (id, option_ids) = {
        let evs = events.lock().unwrap();
        evs.iter()
            .find_map(|e| match e {
                KernelEvent::PermissionRequest { id, options, .. } => {
                    Some((*id, options.iter().map(|o| o.id.clone()).collect::<Vec<_>>()))
                }
                _ => None,
            })
            .unwrap()
    };
    assert!(option_ids.contains(&"allow_once".to_string()));
    assert!(option_ids.contains(&"reject_once".to_string()));
    kernel.resolve_permission(id, Some(choose.to_string())).expect("resolve permission");
    // 同一请求不能应答两次
    assert!(kernel.resolve_permission(id, None).is_err());

    wait_for(&events, 20_000, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    kernel.shutdown();

    events.lock().unwrap().clone()
}

fn assistant_text(events: &[KernelEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            KernelEvent::AgentMessageChunk { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn interactive_permission_allow() {
    let events = run_interactive("allow_once");
    assert!(assistant_text(&events).contains("授权=true"), "{events:?}");
    let status = events.iter().find_map(|e| match e {
        KernelEvent::ToolCallUpdated { status, .. } => status.clone(),
        _ => None,
    });
    assert_eq!(status.as_deref(), Some("completed"));
}

#[test]
fn interactive_permission_reject() {
    let events = run_interactive("reject_once");
    assert!(assistant_text(&events).contains("授权=false"), "{events:?}");
    let status = events.iter().find_map(|e| match e {
        KernelEvent::ToolCallUpdated { status, .. } => status.clone(),
        _ => None,
    });
    assert_eq!(status.as_deref(), Some("failed"));
}

/// 轮询等待直到谓词命中或超时（毫秒）。
fn wait_for(
    events: &Arc<Mutex<Vec<KernelEvent>>>,
    timeout_ms: u64,
    pred: impl Fn(&KernelEvent) -> bool,
) {
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

/// 续接已有会话：按 agent 能力走 resume / load / 退回新建。
/// 返回 (Started 的 sessionId, resumed, 续接后到首轮结束前收到的 agent 文本)。
fn run_resume(mock_flag: &str) -> (String, bool, String) {
    let events: Arc<Mutex<Vec<KernelEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink: EventSink = {
        let events = events.clone();
        Arc::new(move |routed: RoutedEvent| events.lock().unwrap().push(routed.event))
    };
    let kernel = AcpKernel::spawn(
        AcpKernelConfig {
            command: format!("{} {mock_flag}", mock_command()),
            cwd: workspace_root(),
            debug_wire: false,
            auto_approve: true,
        },
        sink,
    )
    .expect("spawn kernel");

    wait_for(&events, 10_000, |e| matches!(e, KernelEvent::Connected { .. }));
    kernel.resume_session(5, "old-session-42", None, &[]).expect("resume session");
    wait_for(&events, 10_000, |e| matches!(e, KernelEvent::Started { .. }));
    let (sid, resumed) = events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            KernelEvent::Started { token: 5, session_id, resumed, .. } => {
                Some((session_id.clone(), *resumed))
            }
            _ => None,
        })
        .expect("started");
    kernel.send_prompt(&sid, UserPrompt::text("继续")).expect("send prompt");
    wait_for(&events, 20_000, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    kernel.shutdown();
    let text = assistant_text(&events.lock().unwrap());
    (sid, resumed, text)
}

#[test]
fn resume_uses_session_resume() {
    let (sid, resumed, text) = run_resume("--resume=resume");
    assert_eq!(sid, "old-session-42");
    assert!(resumed);
    assert!(!text.contains("历史回放"), "{text}");
}

#[test]
fn resume_via_load_drops_replayed_history() {
    let (sid, resumed, text) = run_resume("--resume=load");
    assert_eq!(sid, "old-session-42");
    assert!(resumed);
    assert!(!text.contains("历史回放"), "replayed history leaked: {text}");
    assert!(text.contains("mock 内核"), "new turn must still stream: {text}");
}

#[test]
fn resume_falls_back_to_new_session() {
    let (sid, resumed, _) = run_resume("");
    assert_ne!(sid, "old-session-42");
    assert!(!resumed);
}

/// 检查点：agent 经客户端 `fs/write_text_file` 写的文件，前后内容作为 `FileChanged` 上报。
#[test]
fn client_writes_report_file_changes() {
    let events: Arc<Mutex<Vec<KernelEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink: EventSink = {
        let events = events.clone();
        Arc::new(move |routed: RoutedEvent| events.lock().unwrap().push(routed.event))
    };
    let dir = std::env::temp_dir().join(format!("claudesktop-acp-write-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.txt");
    std::fs::write(&file, "旧").unwrap();
    let kernel = AcpKernel::spawn(
        AcpKernelConfig {
            command: mock_command(),
            cwd: workspace_root(),
            debug_wire: false,
            auto_approve: true,
        },
        sink,
    )
    .expect("spawn kernel");
    let sid = start_session(&kernel, &events);
    let prompt = format!("write:{}:新", file.display());
    kernel.send_prompt(&sid, UserPrompt::text(prompt)).expect("send prompt");
    wait_for(&events, 20_000, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    kernel.shutdown();

    assert_eq!(std::fs::read_to_string(&file).unwrap(), "新");
    let changed = events.lock().unwrap().iter().find_map(|e| match e {
        KernelEvent::FileChanged { before, after, .. } => Some((before.clone(), after.clone())),
        _ => None,
    });
    assert_eq!(changed, Some((Some("旧".into()), Some("新".into()))));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 启用的插件带的 MCP 服务在开会话时交给 agent。
#[test]
fn plugin_mcp_servers_are_passed_to_new_sessions() {
    let events: Arc<Mutex<Vec<KernelEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink: EventSink = {
        let events = events.clone();
        Arc::new(move |routed: RoutedEvent| events.lock().unwrap().push(routed.event))
    };
    let kernel = AcpKernel::spawn(
        AcpKernelConfig {
            command: mock_command(),
            cwd: workspace_root(),
            debug_wire: false,
            auto_approve: true,
        },
        sink,
    )
    .expect("spawn kernel");
    kernel.set_plugins(vec![claudesktop_core::Plugin {
        name: "kit".into(),
        mcp_servers: vec![claudesktop_core::plugin::PluginMcpServer {
            name: "db".into(),
            command: "node".into(),
            args: vec!["server.js".into()],
            ..Default::default()
        }],
        ..Default::default()
    }]);
    let sid = start_session(&kernel, &events);
    kernel.send_prompt(&sid, UserPrompt::text("mcp?")).expect("send prompt");
    wait_for(&events, 20_000, |e| matches!(e, KernelEvent::TurnEnded { .. }));
    kernel.shutdown();
    assert_eq!(assistant_text(&events.lock().unwrap()), "mcp servers: db");
}
