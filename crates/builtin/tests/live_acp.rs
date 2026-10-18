//! 真实 ACP agent 的端到端检查（默认忽略，`cargo test -p claudesktop-builtin --test live_acp -- --ignored`）：
//! agent 经用量网关发请求、日志按会话和轮归组；关掉内核再续接，上下文还在。
//!
//! - `ACP_AGENT`：agent 启动命令，默认 Claude 适配器（`cmd /c npx -y @agentclientprotocol/claude-agent-acp`）
//! - 真实端点取 agent 自己认的 `ANTHROPIC_BASE_URL`（没设就是官方端点），模型、密钥等也照 agent 的环境变量来

use claudesktop_builtin::gateway;
use claudesktop_core::{
    AcpKernel, AcpKernelConfig, EventSink, Kernel, KernelEvent, RoutedEvent, UserPrompt,
};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Events = Arc<Mutex<Vec<KernelEvent>>>;

fn spawn(cwd: &Path, gateway: &gateway::GatewayHandle) -> (AcpKernel, Events) {
    let command = std::env::var("ACP_AGENT")
        .unwrap_or_else(|_| "cmd /c npx -y @agentclientprotocol/claude-agent-acp".into());
    let events: Events = Arc::default();
    let sink: EventSink = {
        let events = events.clone();
        Arc::new(move |routed: RoutedEvent| events.lock().unwrap().push(routed.event))
    };
    let config = AcpKernelConfig {
        command,
        cwd: cwd.to_path_buf(),
        debug_wire: std::env::var("ACP_DEBUG_WIRE").is_ok_and(|v| v == "1"),
        auto_approve: true,
        env: gateway::agent_env(gateway),
    };
    let kernel = AcpKernel::spawn(config, sink).expect("spawn agent");
    wait(&events, 120, "connected", |e| matches!(e, KernelEvent::Connected { .. }));
    (kernel, events)
}

fn wait(events: &Events, secs: u64, what: &str, pred: impl Fn(&KernelEvent) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        {
            let events = events.lock().unwrap();
            if events.iter().any(&pred) {
                return;
            }
            if let Some(e) = events.iter().find(|e| {
                matches!(
                    e,
                    KernelEvent::SessionFailed { .. }
                        | KernelEvent::PromptFailed { .. }
                        | KernelEvent::Fatal(_)
                )
            }) {
                panic!("等 {what} 时出错：{e:?}");
            }
        }
        assert!(Instant::now() < deadline, "等不到 {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 发一条消息、等这一轮结束，返回 agent 回的文字。
fn turn(kernel: &AcpKernel, events: &Events, sid: &str, n: u64, text: &str) -> String {
    events.lock().unwrap().clear();
    gateway::note_turn(sid, n);
    kernel.send_prompt(sid, UserPrompt::text(text)).unwrap();
    wait(events, 180, "turn end", |e| matches!(e, KernelEvent::TurnEnded { .. }));
    events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            KernelEvent::AgentMessageChunk { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn started(events: &Events, token: u64) -> (String, bool) {
    wait(
        events,
        120,
        "started",
        |e| matches!(e, KernelEvent::Started { token: t, .. } if *t == token),
    );
    events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            KernelEvent::Started { token: t, session_id, resumed, .. } if *t == token => {
                Some((session_id.clone(), *resumed))
            }
            _ => None,
        })
        .unwrap()
}

#[test]
#[ignore]
fn live_agent_through_gateway_and_resume() {
    let root = std::env::temp_dir().join(format!("claudesktop-live-acp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (cwd, logs) = (root.join("work"), root.join("gateway"));
    std::fs::create_dir_all(&cwd).unwrap();
    let gw = gateway::Gateway::start(logs.clone()).unwrap();

    // 第一轮：新会话
    let (kernel, events) = spawn(&cwd, &gw);
    kernel.new_session(1, None).unwrap();
    let (sid, _) = started(&events, 1);
    let reply = turn(&kernel, &events, &sid, 1, "Remember the word PAPAYA. Reply with just: ok");
    println!("第 1 轮：{reply}");
    assert!(!reply.trim().is_empty());
    kernel.shutdown();

    let first = gateway::read_logs(&logs, None);
    for e in &first {
        println!(
            "  {} {} session={:?} turn={:?} status={} in={} out={} cache={} {}ms",
            e.purpose,
            e.path,
            e.session,
            e.turn,
            e.status,
            e.input,
            e.output,
            e.cache_read,
            e.total_ms
        );
    }
    let ours: Vec<_> =
        first.iter().filter(|e| e.session.as_deref() == Some(sid.as_str())).collect();
    assert!(!ours.is_empty(), "网关日志里没有对上这个会话的请求");
    assert!(ours.iter().all(|e| e.purpose == "acp" && e.turn == Some(1)));
    assert!(ours.iter().any(|e| e.status == 200 && e.output > 0));

    // 第二轮：新起一个 agent 进程续接，看上下文还在不在
    let (kernel, events) = spawn(&cwd, &gw);
    kernel.resume_session(2, &sid, None, &[]).unwrap();
    let (resumed_sid, resumed) = started(&events, 2);
    println!("续接：resumed={resumed} 同一会话={}", resumed_sid == sid);
    let reply = turn(
        &kernel,
        &events,
        &resumed_sid,
        2,
        "What word did I ask you to remember? Reply with just the word.",
    );
    println!("第 2 轮：{reply}");
    kernel.shutdown();
    assert!(resumed && resumed_sid == sid, "agent 不支持续接");
    assert!(reply.to_uppercase().contains("PAPAYA"), "续接后丢了上下文：{reply}");
    let second: Vec<_> = gateway::read_logs(&logs, None)
        .into_iter()
        .filter(|e| e.session.as_deref() == Some(sid.as_str()) && e.turn == Some(2))
        .collect();
    assert!(!second.is_empty(), "续接后的请求没归到第 2 轮");
    let _ = std::fs::remove_dir_all(&root);
}
