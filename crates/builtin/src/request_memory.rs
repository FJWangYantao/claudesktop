use super::*;
use claudesktop_core::UserPrompt;
use serde::Serialize;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
struct RequestReady {
    epoch_ms: u128,
    bytes: usize,
}

static READY: OnceLock<Mutex<Vec<RequestReady>>> = OnceLock::new();

#[derive(Serialize)]
struct GatewayWrite {
    epoch_ms: u128,
    elapsed_ms: f64,
    parse_ms: f64,
}

static GATEWAY_WRITES: OnceLock<Mutex<Vec<GatewayWrite>>> = OnceLock::new();

pub(crate) fn record_gateway_write(started: Instant, parse_ms: f64) {
    if let Some(writes) = GATEWAY_WRITES.get() {
        writes.lock().unwrap().push(GatewayWrite {
            epoch_ms: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(),
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.,
            parse_ms,
        });
    }
}

pub(crate) fn record_request_ready(bytes: usize) {
    if let Some(ready) = READY.get() {
        ready.lock().unwrap().push(RequestReady {
            epoch_ms: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(),
            bytes,
        });
    }
}

#[repr(C)]
#[derive(Default)]
struct MemoryCounters {
    size: u32,
    faults: u32,
    peak_ws: usize,
    ws: usize,
    peak_paged_pool: usize,
    paged_pool: usize,
    peak_nonpaged_pool: usize,
    nonpaged_pool: usize,
    pagefile: usize,
    peak_pagefile: usize,
    private_bytes: usize,
}

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> isize;
    fn GetProcessTimes(
        process: isize,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
}

#[link(name = "psapi")]
unsafe extern "system" {
    fn GetProcessMemoryInfo(process: isize, counters: *mut MemoryCounters, size: u32) -> i32;
}

#[derive(Serialize)]
struct Sample {
    epoch_ms: u128,
    phase: usize,
    pb: usize,
    ws: usize,
    peak_pb: usize,
    peak_ws: usize,
    cpu_ms: f64,
}

fn sample(phase: usize) -> Sample {
    let mut counters =
        MemoryCounters { size: std::mem::size_of::<MemoryCounters>() as u32, ..Default::default() };
    let mut created = FileTime::default();
    let mut exited = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    unsafe {
        let process = GetCurrentProcess();
        assert_ne!(GetProcessMemoryInfo(process, &mut counters, counters.size), 0);
        assert_ne!(GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user), 0);
    }
    let ticks = |time: FileTime| (u64::from(time.high) << 32) | u64::from(time.low);
    Sample {
        epoch_ms: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(),
        phase,
        pb: counters.private_bytes,
        ws: counters.ws,
        peak_pb: counters.peak_pagefile,
        peak_ws: counters.peak_ws,
        cpu_ms: (ticks(kernel) + ticks(user)) as f64 / 10_000.,
    }
}

fn history(count: usize, bytes: usize) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    for i in 0..count {
        messages.push(ChatMessage::new("user", format!("seed request {i}")));
        let mut call = ChatMessage::assistant_tool_calls(
            format!("seed answer {i}"),
            vec![ToolCall {
                id: format!("seed-call-{i}"),
                kind: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"sample.txt"}"#.into(),
                },
            }],
        );
        call.thinking = vec![json!({
            "type": "thinking", "thinking": "signed thought. ".repeat(bytes / 64),
            "signature": format!("seed-signature-{i}"),
        })];
        messages.push(call);
        messages.push(ChatMessage::tool_result(
            format!("seed-call-{i}"),
            format!(
                "seed-result-{i}\n{}",
                "text with a quote \" and a slash \\.\n".repeat(bytes / 36)
            ),
        ));
    }
    messages
}

#[test]
#[ignore]
fn real_request_memory() {
    let base =
        std::env::var("CLAUDESKTOP_REQUEST_BENCH_URL").expect("通过 request-memory-bench.mjs 启动");
    assert!(base.starts_with("http://127.0.0.1:"));
    let directory = PathBuf::from(std::env::var_os("CLAUDESKTOP_REQUEST_BENCH_DIR").unwrap());
    let protocol_name = std::env::var("CLAUDESKTOP_REQUEST_BENCH_PROTOCOL").unwrap();
    let protocol = match protocol_name.as_str() {
        "openai" => Protocol::OpenAiCompat,
        "anthropic" => Protocol::Anthropic,
        "responses" => Protocol::Responses,
        _ => panic!("未知协议"),
    };
    let long = std::env::var("CLAUDESKTOP_REQUEST_BENCH_SIZE").unwrap() == "long";
    let cwd = PathBuf::from(std::env::var_os("CLAUDESKTOP_REQUEST_BENCH_CWD").unwrap());
    let turn_count =
        std::env::var("CLAUDESKTOP_REQUEST_BENCH_TURNS").unwrap().parse::<usize>().unwrap();
    READY.set(Mutex::new(Vec::new())).ok().unwrap();
    let gateway = if std::env::var("CLAUDESKTOP_REQUEST_BENCH_GATEWAY").as_deref() == Ok("1") {
        GATEWAY_WRITES.set(Mutex::new(Vec::new())).ok().unwrap();
        crate::request_bodies::set_enabled(
            std::env::var("CLAUDESKTOP_REQUEST_BENCH_SAVE").as_deref() != Ok("off"),
        );
        Some(crate::gateway::Gateway::start(directory.join("gateway")).unwrap())
    } else {
        None
    };
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join("sample.txt"), "roundtrip sample\n").unwrap();
    let ended = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let sink: EventSink = {
        let ended = ended.clone();
        let failures = failures.clone();
        Arc::new(move |event| match event.event {
            KernelEvent::TurnEnded { .. } => {
                ended.fetch_add(1, Ordering::SeqCst);
            }
            KernelEvent::PromptFailed { message } => failures.lock().unwrap().push(message),
            _ => {}
        })
    };
    let mut config = BuiltinConfig::new(
        ProviderConfig {
            protocol,
            base_url: base,
            model: "request-bench".into(),
            gateway: gateway.clone(),
            ..Default::default()
        },
        cwd,
    );
    config.rules_path = None;
    config.user_instructions = None;
    config.mcp_config = None;
    config.memory_path = None;
    config.session_titles = false;
    config.hibernation_dir = None;
    config.max_steps = 5;
    let kernel = BuiltinKernel::spawn(config, sink).unwrap();
    kernel.new_session(1, SessionSpec::default()).unwrap();
    let sid = kernel.inner.sessions.lock().unwrap().keys().next().unwrap().clone();
    let history_bytes = {
        let mut sessions = kernel.inner.sessions.lock().unwrap();
        let state = sessions.get_mut(&sid).unwrap();
        state.mode = MODE_BYPASS.into();
        state.history = if long { history(48, 256 * 1024) } else { history(2, 512) };
        hibernate::estimated_bytes(&state.history)
    };
    let before = sample(0);
    let phase = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let stop = stop.clone();
        let phase = phase.clone();
        std::thread::spawn(move || {
            let mut samples = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                samples.push(sample(phase.load(Ordering::SeqCst)));
                std::thread::sleep(Duration::from_millis(5));
            }
            samples
        })
    };
    let mut turns = Vec::new();
    for turn in 0..turn_count {
        phase.store(turn * 2 + 1, Ordering::SeqCst);
        let epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
        let started = Instant::now();
        kernel.send_prompt(&sid, UserPrompt::text(format!("benchmark turn {turn}"))).unwrap();
        while ended.load(Ordering::SeqCst) <= turn
            || GATEWAY_WRITES
                .get()
                .is_some_and(|writes| writes.lock().unwrap().len() < (turn + 1) * 3)
        {
            assert!(failures.lock().unwrap().is_empty(), "{:?}", failures.lock().unwrap());
            assert!(started.elapsed() < Duration::from_secs(90), "请求超时");
            std::thread::sleep(Duration::from_millis(2));
        }
        turns.push(
            json!({ "sent_at_ms": epoch, "elapsed_ms": started.elapsed().as_secs_f64() * 1000. }),
        );
        phase.store(turn * 2 + 2, Ordering::SeqCst);
        if std::env::var("CLAUDESKTOP_REQUEST_BENCH_OBSERVER").as_deref() == Ok("idle") {
            use std::io::{Read, Write};
            println!("GATEWAY_BENCH_IDLE {}", turn + 1);
            std::io::stdout().flush().unwrap();
            let mut acknowledged = [0];
            std::io::stdin().read_exact(&mut acknowledged).unwrap();
            assert_eq!(acknowledged, [b'\n']);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::SeqCst);
    let samples = sampler.join().unwrap();
    let after = sample(turn_count * 2 + 1);
    let request_ready = std::mem::take(&mut *READY.get().unwrap().lock().unwrap());
    let gateway_writes = GATEWAY_WRITES
        .get()
        .map(|writes| std::mem::take(&mut *writes.lock().unwrap()))
        .unwrap_or_default();
    let gateway_logs = gateway.as_ref().map(|g| crate::gateway::read_logs(g.dir(), None));
    assert_eq!(request_ready.len(), turn_count * 3);
    if gateway.is_some() {
        assert_eq!(gateway_writes.len(), turn_count * 3);
        assert_eq!(gateway_logs.as_ref().unwrap().len(), turn_count * 3);
    }
    kernel.shutdown();
    assert!(failures.lock().unwrap().is_empty());
    std::fs::write(
        directory.join("metrics.json"),
        serde_json::to_vec_pretty(&json!({
            "protocol": protocol_name, "size": if long { "long" } else { "short" },
            "history_bytes": history_bytes, "before": before, "after": after,
            "samples": samples, "turns": turns, "request_ready": request_ready,
            "session": sid, "gateway_writes": gateway_writes, "gateway_logs": gateway_logs,
        }))
        .unwrap(),
    )
    .unwrap();
}
