//! 闲置会话的完整历史快照：无损编解码、原子读写和内核侧存储状态机。

use crate::provider::{ChatMessage, ContentPart, FunctionCall, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotLocation {
    pub path: PathBuf,
    pub generation: u64,
    pub bytes: usize,
}

pub(crate) struct WriteOutcome {
    pub location: SnapshotLocation,
    pub serialize: Duration,
    pub write: Duration,
}

pub(crate) struct ReadOutcome {
    pub messages: Vec<ChatMessage>,
    pub read: Duration,
    pub parse: Duration,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    session: String,
    generation: u64,
    messages: Vec<MessageSnapshot>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageSnapshot {
    role: String,
    parts: Vec<PartSnapshot>,
    tool_calls: Vec<ToolCallSnapshot>,
    tool_call_id: Option<String>,
    thinking: Vec<Value>,
    tool_images: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum PartSnapshot {
    Text(String),
    Image { media_type: String, data: String },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCallSnapshot {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: FunctionSnapshot,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FunctionSnapshot {
    name: String,
    arguments: String,
}

impl MessageSnapshot {
    fn from_message(message: &ChatMessage) -> Self {
        Self {
            role: message.role.clone(),
            parts: message
                .parts
                .iter()
                .map(|part| match part {
                    ContentPart::Text(text) => PartSnapshot::Text(text.clone()),
                    ContentPart::Image { media_type, data } => {
                        PartSnapshot::Image { media_type: media_type.clone(), data: data.clone() }
                    }
                })
                .collect(),
            tool_calls: message
                .tool_calls
                .iter()
                .map(|call| ToolCallSnapshot {
                    id: call.id.clone(),
                    kind: call.kind.clone(),
                    function: FunctionSnapshot {
                        name: call.function.name.clone(),
                        arguments: call.function.arguments.clone(),
                    },
                })
                .collect(),
            tool_call_id: message.tool_call_id.clone(),
            thinking: message.thinking.clone(),
            tool_images: message.tool_images,
        }
    }

    fn into_message(self) -> ChatMessage {
        ChatMessage {
            role: self.role,
            parts: self
                .parts
                .into_iter()
                .map(|part| match part {
                    PartSnapshot::Text(text) => ContentPart::Text(text),
                    PartSnapshot::Image { media_type, data } => {
                        ContentPart::Image { media_type, data }
                    }
                })
                .collect(),
            tool_calls: self
                .tool_calls
                .into_iter()
                .map(|call| ToolCall {
                    id: call.id,
                    kind: call.kind,
                    function: FunctionCall {
                        name: call.function.name,
                        arguments: call.function.arguments,
                    },
                })
                .collect(),
            tool_call_id: self.tool_call_id,
            thinking: self.thinking,
            tool_images: self.tool_images,
        }
    }
}

fn encode(session: &str, generation: u64, messages: &[ChatMessage]) -> anyhow::Result<Vec<u8>> {
    let snapshot = Snapshot {
        version: VERSION,
        session: session.to_string(),
        generation,
        messages: messages.iter().map(MessageSnapshot::from_message).collect(),
    };
    serde_json::to_vec(&snapshot).map_err(Into::into)
}

fn decode(bytes: &[u8], session: &str, generation: u64) -> anyhow::Result<Vec<ChatMessage>> {
    let snapshot: Snapshot = serde_json::from_slice(bytes)
        .map_err(|err| anyhow::anyhow!("历史快照不是完整有效的 JSON：{err}"))?;
    anyhow::ensure!(snapshot.version == VERSION, "不支持的历史快照版本 {}", snapshot.version);
    anyhow::ensure!(snapshot.session == session, "历史快照属于另一个会话");
    anyhow::ensure!(snapshot.generation == generation, "历史快照代次不一致");
    Ok(snapshot.messages.into_iter().map(MessageSnapshot::into_message).collect())
}

pub(crate) fn write(
    root: &Path,
    session: &str,
    generation: u64,
    messages: &[ChatMessage],
) -> anyhow::Result<WriteOutcome> {
    let serialize_started = Instant::now();
    let bytes = encode(session, generation, messages)?;
    let serialize = serialize_started.elapsed();
    let write_started = Instant::now();
    std::fs::create_dir_all(root)?;
    let base = format!("{session}-{generation}");
    let final_path = root.join(format!("{base}.json"));
    let temporary_path = root.join(format!("{base}.tmp"));
    let result = (|| -> anyhow::Result<()> {
        let mut file =
            std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary_path, &final_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result?;
    Ok(WriteOutcome {
        location: SnapshotLocation { path: final_path, generation, bytes: bytes.len() },
        serialize,
        write: write_started.elapsed(),
    })
}

pub(crate) fn read(session: &str, location: &SnapshotLocation) -> anyhow::Result<ReadOutcome> {
    let read_started = Instant::now();
    let bytes = std::fs::read(&location.path)?;
    let read = read_started.elapsed();
    anyhow::ensure!(
        bytes.len() == location.bytes,
        "历史快照大小已改变（应为 {} 字节，实际 {} 字节）",
        location.bytes,
        bytes.len()
    );
    let parse_started = Instant::now();
    let messages = decode(&bytes, session, location.generation)?;
    Ok(ReadOutcome { messages, read, parse: parse_started.elapsed() })
}

pub(crate) fn remove(location: &SnapshotLocation) {
    if let Err(err) = std::fs::remove_file(&location.path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("[perf] session_hibernate_cleanup error={err}");
    }
}

pub(crate) fn estimated_bytes(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|message| {
            let parts: usize = message
                .parts
                .iter()
                .map(|part| match part {
                    ContentPart::Text(text) => text.capacity(),
                    ContentPart::Image { media_type, data } => {
                        media_type.capacity() + data.capacity()
                    }
                })
                .sum();
            let calls: usize = message
                .tool_calls
                .iter()
                .map(|call| {
                    call.id.capacity()
                        + call.kind.capacity()
                        + call.function.name.capacity()
                        + call.function.arguments.capacity()
                })
                .sum();
            let thinking = message.thinking.iter().map(json_bytes).sum::<usize>();
            std::mem::size_of::<ChatMessage>()
                + message.role.capacity()
                + message.parts.capacity() * std::mem::size_of::<ContentPart>()
                + parts
                + message.tool_calls.capacity() * std::mem::size_of::<ToolCall>()
                + calls
                + message.tool_call_id.as_ref().map_or(0, String::capacity)
                + message.thinking.capacity() * std::mem::size_of::<Value>()
                + thinking
        })
        .sum()
}

fn json_bytes(value: &Value) -> usize {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
        Value::String(text) => text.capacity(),
        Value::Array(items) => {
            items.capacity() * std::mem::size_of::<Value>()
                + items.iter().map(json_bytes).sum::<usize>()
        }
        Value::Object(map) => {
            map.iter().map(|(key, value)| key.capacity() + json_bytes(value)).sum()
        }
    }
}

impl super::Inner {
    pub(crate) fn begin_history_activity(&self, session: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(state) = sessions.get_mut(session) else {
            return false;
        };
        state.history_activity += 1;
        true
    }

    pub(crate) fn end_history_activity(&self, session: &str) {
        if let Some(state) = self.sessions.lock().unwrap().get_mut(session) {
            state.history_activity = state.history_activity.saturating_sub(1);
        }
    }

    pub(crate) fn history_io(
        &self,
        session: &str,
    ) -> anyhow::Result<std::sync::Arc<std::sync::Mutex<()>>> {
        self.sessions
            .lock()
            .unwrap()
            .get(session)
            .map(|state| state.history_io.clone())
            .ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))
    }

    pub(crate) fn wake_history(&self, session: &str) -> anyhow::Result<()> {
        let lock_started = Instant::now();
        let io = self.history_io(session)?;
        let _guard = io.lock().unwrap();
        self.wake_history_locked(session, lock_started.elapsed())
    }

    /// 调用方必须已经持有 `history_io`；磁盘读取期间不持全局会话锁。
    pub(crate) fn wake_history_locked(&self, session: &str, wait: Duration) -> anyhow::Result<()> {
        let location = {
            let mut sessions = self.sessions.lock().unwrap();
            let state = sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("会话已关闭"))?;
            match &state.history_storage {
                super::HistoryStorage::Resident => return Ok(()),
                super::HistoryStorage::Dormant(location) => {
                    let location = location.clone();
                    state.history_storage = super::HistoryStorage::Restoring(location.clone());
                    location
                }
                super::HistoryStorage::Spilling(_) | super::HistoryStorage::Restoring(_) => {
                    anyhow::bail!("历史存储状态冲突，请重试")
                }
            }
        };
        let total_started = Instant::now();
        match read(session, &location) {
            Ok(outcome) => {
                let mut sessions = self.sessions.lock().unwrap();
                let Some(state) = sessions.get_mut(session) else {
                    drop(sessions);
                    remove(&location);
                    anyhow::bail!("会话已关闭")
                };
                state.history = outcome.messages;
                state.history_storage = super::HistoryStorage::Resident;
                drop(sessions);
                remove(&location);
                eprintln!(
                    "[perf] session_wake bytes={} read_ms={} parse_ms={} wait_ms={} total_ms={}",
                    location.bytes,
                    outcome.read.as_secs_f64() * 1000.,
                    outcome.parse.as_secs_f64() * 1000.,
                    wait.as_secs_f64() * 1000.,
                    total_started.elapsed().as_secs_f64() * 1000.
                );
                Ok(())
            }
            Err(err) => {
                if let Some(state) = self.sessions.lock().unwrap().get_mut(session)
                    && matches!(&state.history_storage, super::HistoryStorage::Restoring(_))
                {
                    state.history_storage = super::HistoryStorage::Dormant(location.clone());
                }
                eprintln!(
                    "[perf] session_wake bytes={} wait_ms={} error={}",
                    location.bytes,
                    wait.as_secs_f64() * 1000.,
                    err
                );
                Err(anyhow::anyhow!(
                    "恢复完整历史失败：{err:#}；快照仍保留在 {}，可修复后重试",
                    location.path.display()
                ))
            }
        }
    }

    pub(crate) fn hibernate_history(&self, session: &str) -> anyhow::Result<()> {
        let Some(root) = self.hibernation_dir.clone() else {
            return Ok(());
        };
        let io = self
            .sessions
            .lock()
            .unwrap()
            .get(session)
            .map(|state| state.history_io.clone())
            .ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        let _guard = io.lock().unwrap();
        let (history, generation) = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(state) = sessions.get_mut(session) else {
                return Ok(());
            };
            if state.turn.is_some()
                || state.history_activity != 0
                || state.agent.is_some()
                || !matches!(&state.history_storage, super::HistoryStorage::Resident)
                || super::bgtasks::has_running(session)
            {
                return Ok(());
            }
            state.history_generation += 1;
            let generation = state.history_generation;
            state.history_storage = super::HistoryStorage::Spilling(generation);
            (std::mem::take(&mut state.history), generation)
        };
        let total_started = Instant::now();
        match write(&root, session, generation, &history) {
            Ok(outcome) => {
                let location = outcome.location;
                let dormant = {
                    let mut sessions = self.sessions.lock().unwrap();
                    match sessions.get_mut(session) {
                        Some(state)
                            if state.turn.is_none()
                                && state.history_activity == 0
                                && matches!(
                                    &state.history_storage,
                                    super::HistoryStorage::Spilling(g) if *g == generation
                                ) =>
                        {
                            state.history_storage =
                                super::HistoryStorage::Dormant(location.clone());
                            true
                        }
                        Some(state) => {
                            if matches!(
                                &state.history_storage,
                                super::HistoryStorage::Spilling(g) if *g == generation
                            ) {
                                state.history = history;
                                state.history_storage = super::HistoryStorage::Resident;
                            }
                            false
                        }
                        None => false,
                    }
                };
                if dormant {
                    eprintln!(
                        "[perf] session_hibernate bytes={} serialize_ms={} write_ms={} total_ms={}",
                        location.bytes,
                        outcome.serialize.as_secs_f64() * 1000.,
                        outcome.write.as_secs_f64() * 1000.,
                        total_started.elapsed().as_secs_f64() * 1000.
                    );
                } else {
                    remove(&location);
                }
                Ok(())
            }
            Err(err) => {
                if let Some(state) = self.sessions.lock().unwrap().get_mut(session)
                    && matches!(
                        &state.history_storage,
                        super::HistoryStorage::Spilling(g) if *g == generation
                    )
                {
                    state.history = history;
                    state.history_storage = super::HistoryStorage::Resident;
                }
                eprintln!("[perf] session_hibernate error={err}");
                Err(anyhow::anyhow!("闲置历史快照写入失败，已恢复内存中的原历史：{err:#}"))
            }
        }
    }

    pub(crate) fn history_stats(&self) -> claudesktop_core::KernelHistoryStats {
        let mut stats = claudesktop_core::KernelHistoryStats::default();
        for state in self.sessions.lock().unwrap().values() {
            match &state.history_storage {
                super::HistoryStorage::Resident => {
                    stats.resident_sessions += 1;
                    stats.resident_bytes += state.history.capacity()
                        * std::mem::size_of::<ChatMessage>()
                        + estimated_bytes(&state.history);
                }
                super::HistoryStorage::Dormant(_) => stats.dormant_sessions += 1,
                super::HistoryStorage::Spilling(_) | super::HistoryStorage::Restoring(_) => {
                    stats.transitioning_sessions += 1;
                }
            }
        }
        stats
    }

    pub(crate) fn close_history(&self, session: &str) {
        let state = self.sessions.lock().unwrap().remove(session);
        let (Some(state), Some(root)) = (state, self.hibernation_dir.clone()) else {
            return;
        };
        let session = session.to_string();
        std::thread::spawn(move || {
            let _guard = state.history_io.lock().unwrap();
            remove_session_files(&root, &session);
        });
    }

    pub(crate) fn shutdown_history(&self) {
        let states = self.sessions.lock().unwrap().drain().collect();
        cleanup_history(states, self.hibernation_dir.clone());
    }
}

fn cleanup_history(states: Vec<(String, super::SessionState)>, root: Option<PathBuf>) {
    let Some(root) = root else {
        return;
    };
    std::thread::spawn(move || {
        for (session, state) in states {
            let _guard = state.history_io.lock().unwrap();
            remove_storage(&state.history_storage);
            remove_session_files(&root, &session);
        }
    });
}

fn remove_session_files(root: &Path, session: &str) {
    let prefix = format!("{session}-");
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && (name.ends_with(".json") || name.ends_with(".tmp")) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn remove_storage(storage: &super::HistoryStorage) {
    match storage {
        super::HistoryStorage::Dormant(location) | super::HistoryStorage::Restoring(location) => {
            remove(location)
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_history() -> Vec<ChatMessage> {
        let user = ChatMessage::with_parts(
            "user",
            vec![
                ContentPart::Text("先看图，再调用工具。".into()),
                ContentPart::Image { media_type: "image/png".into(), data: "QUJD".into() },
                ContentPart::Text("分段顺序也要保留".into()),
            ],
        );
        let mut assistant = ChatMessage::assistant_tool_calls(
            "准备读取".into(),
            vec![ToolCall {
                id: "call-1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"notes.txt","optional":null}"#.into(),
                },
            }],
        );
        assistant.thinking = vec![
            json!({"type":"thinking","thinking":"完整思考","signature":"sig-1"}),
            json!({"type":"redacted_thinking","data":"opaque-signature"}),
        ];
        let mut tool = ChatMessage::tool_result_with_images(
            "call-1".into(),
            "完整工具输出\n第二行".into(),
            vec![crate::tools::OutputImage {
                media_type: "image/jpeg".into(),
                data: "REVG".into(),
            }],
        );
        tool.tool_images = true;
        vec![user, assistant, tool, ChatMessage::new("assistant", "完成")]
    }

    #[test]
    fn round_trip_preserves_every_field_and_provider_messages() {
        let original = full_history();
        let bytes = encode("builtin-1-1", 7, &original).unwrap();
        let restored = decode(&bytes, "builtin-1-1", 7).unwrap();
        assert_eq!(restored, original);
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&original).unwrap(),
            "OpenAI 请求消息逐项一致"
        );
        assert_eq!(
            crate::anthropic::convert_messages(&restored),
            crate::anthropic::convert_messages(&original),
            "Anthropic 请求消息（含思考签名）逐项一致"
        );
    }

    #[test]
    fn atomic_write_and_read_round_trip() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "claudesktop-hibernate-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let original = full_history();
        let written = write(&root, "builtin-test-1", 1, &original).unwrap();
        let outcome = read("builtin-test-1", &written.location).unwrap();
        assert_eq!(outcome.messages, original);
        assert!(!root.join("builtin-test-1-1.tmp").exists());

        let bytes = std::fs::read(&written.location.path).unwrap();
        std::fs::write(&written.location.path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(read("builtin-test-1", &written.location).is_err());
        assert!(written.location.path.exists(), "读取失败不能删掉原快照");
        remove(&written.location);
        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Debug, Default, Clone, Copy)]
    struct ProcessMemory {
        private_bytes: u64,
        working_set: u64,
    }

    #[cfg(windows)]
    fn process_memory() -> ProcessMemory {
        let script = format!(
            "$p=Get-Process -Id {}; Write-Output \"$($p.PrivateMemorySize64),$($p.WorkingSet64)\"",
            std::process::id()
        );
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .expect("读取进程内存");
        let text = String::from_utf8_lossy(&output.stdout);
        let mut values = text.trim().split(',').filter_map(|value| value.parse::<u64>().ok());
        ProcessMemory {
            private_bytes: values.next().unwrap_or_default(),
            working_set: values.next().unwrap_or_default(),
        }
    }

    #[cfg(not(windows))]
    fn process_memory() -> ProcessMemory {
        ProcessMemory::default()
    }

    fn median(values: &mut [u64]) -> u64 {
        values.sort_unstable();
        values.get(values.len() / 2).copied().unwrap_or_default()
    }

    fn p95(values: &mut [u64]) -> u64 {
        values.sort_unstable();
        values.get(values.len().saturating_sub(1) * 95 / 100).copied().unwrap_or_default()
    }

    fn mib(bytes: u64) -> f64 {
        bytes as f64 / 1048576.
    }

    fn mib_difference(before: u64, after: u64) -> f64 {
        (before as f64 - after as f64) / 1048576.
    }

    fn memory_sample(round: usize, phase: &str) -> ProcessMemory {
        std::thread::sleep(Duration::from_millis(250));
        let memory = process_memory();
        eprintln!(
            "[perf-ab] round={} phase={} private_mb={:.1} working_mb={:.1}",
            round,
            phase,
            mib(memory.private_bytes),
            mib(memory.working_set)
        );
        memory
    }

    fn record_peak(peak: &mut ProcessMemory) {
        let memory = process_memory();
        peak.private_bytes = peak.private_bytes.max(memory.private_bytes);
        peak.working_set = peak.working_set.max(memory.working_set);
    }

    fn hibernate_all(kernel: &crate::BuiltinKernel, sessions: &[String], peak: &mut ProcessMemory) {
        for (index, sid) in sessions.iter().enumerate() {
            kernel.inner.hibernate_history(sid).unwrap();
            if index % 5 == 4 {
                record_peak(peak);
            }
        }
        assert_eq!(kernel.inner.history_stats().dormant_sessions, 50);
    }

    fn wake_all(
        kernel: &crate::BuiltinKernel,
        sessions: &[String],
        peak: &mut ProcessMemory,
    ) -> (Vec<u64>, Vec<u64>) {
        let mut wakes = Vec::new();
        let mut waits = Vec::new();
        for (index, sid) in sessions.iter().enumerate() {
            let started = Instant::now();
            kernel.inner.wake_history(sid).unwrap();
            wakes.push(started.elapsed().as_micros() as u64);
            let started = Instant::now();
            kernel.inner.wake_history(sid).unwrap();
            waits.push(started.elapsed().as_micros() as u64);
            if index % 5 == 4 {
                record_peak(peak);
            }
        }
        (wakes, waits)
    }

    /// 手动性能验收：`cargo test -p claudesktop-builtin hibernation_ab_memory --release -- --ignored --nocapture`。
    #[test]
    #[ignore]
    fn hibernation_ab_memory_and_latency() {
        use claudesktop_core::{EventSink, Kernel, SessionSpec};
        use std::sync::Arc;

        let root =
            std::env::temp_dir().join(format!("claudesktop-hibernate-ab-{}", std::process::id()));
        let cwd = root.join("cwd");
        let snapshots = root.join("snapshots");
        std::fs::create_dir_all(&cwd).unwrap();
        let sink: EventSink = Arc::new(|_| {});
        let mut config = crate::BuiltinConfig::new(
            crate::ProviderConfig { model: "perf-model".into(), ..Default::default() },
            cwd,
        );
        config.rules_path = None;
        config.user_instructions = None;
        config.mcp_config = None;
        config.memory_path = None;
        config.hibernation_dir = Some(snapshots.clone());
        let kernel = crate::BuiltinKernel::spawn(config, sink).unwrap();
        let mut sessions = Vec::new();
        for index in 1..=50u64 {
            kernel.new_session(index, SessionSpec::default()).unwrap();
            let sid = format!("builtin-{}-{index}", std::process::id());
            let mut assistant = ChatMessage::assistant_tool_calls(
                "r".repeat(256 * 1024),
                vec![ToolCall {
                    id: "perf-call".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "perf_tool".into(),
                        arguments: format!(r#"{{"payload":"{}"}}"#, "a".repeat(256 * 1024)),
                    },
                }],
            );
            assistant.thinking = vec![serde_json::json!({
                "type": "thinking",
                "thinking": "t".repeat(256 * 1024),
                "signature": "perf-signature"
            })];
            let tool = ChatMessage::tool_result_with_images(
                "perf-call".into(),
                "o".repeat(512 * 1024),
                vec![crate::tools::OutputImage {
                    media_type: "image/png".into(),
                    data: "A".repeat(512 * 1024),
                }],
            );
            let state_history = vec![
                ChatMessage::new("user", "u".repeat(256 * 1024)),
                assistant,
                tool,
                ChatMessage::new("assistant", "f".repeat(256 * 1024)),
            ];
            let mut states = kernel.inner.sessions.lock().unwrap();
            let state = states.get_mut(&sid).unwrap();
            state.history = state_history;
            state.summary = Some("保留摘要".into());
            state.measured = Some((1234, 1));
            drop(states);
            sessions.push(sid);
        }
        assert_eq!(kernel.inner.history_stats().resident_sessions, 50);

        let mut peak = ProcessMemory::default();
        let mut resident_private = Vec::new();
        let mut resident_working = Vec::new();
        let mut dormant_private = Vec::new();
        let mut dormant_working = Vec::new();
        let mut wakes = Vec::new();
        let mut waits = Vec::new();
        for round in 0..3 {
            if round % 2 == 1 {
                hibernate_all(&kernel, &sessions, &mut peak);
                let memory = memory_sample(round, "dormant");
                dormant_private.push(memory.private_bytes);
                dormant_working.push(memory.working_set);
                let (mut w, mut p) = wake_all(&kernel, &sessions, &mut peak);
                wakes.append(&mut w);
                waits.append(&mut p);
                let memory = memory_sample(round, "resident");
                resident_private.push(memory.private_bytes);
                resident_working.push(memory.working_set);
            } else {
                let memory = memory_sample(round, "resident");
                resident_private.push(memory.private_bytes);
                resident_working.push(memory.working_set);
                hibernate_all(&kernel, &sessions, &mut peak);
                let memory = memory_sample(round, "dormant");
                dormant_private.push(memory.private_bytes);
                dormant_working.push(memory.working_set);
                if round == 2 {
                    for (index, sid) in sessions.iter().take(25).enumerate() {
                        let started = Instant::now();
                        kernel.inner.wake_history(sid).unwrap();
                        wakes.push(started.elapsed().as_micros() as u64);
                        if index % 5 == 4 {
                            record_peak(&mut peak);
                        }
                    }
                    memory_sample(round, "half-restored");
                    for (index, sid) in sessions.iter().skip(25).enumerate() {
                        let started = Instant::now();
                        kernel.inner.wake_history(sid).unwrap();
                        wakes.push(started.elapsed().as_micros() as u64);
                        if index % 5 == 4 {
                            record_peak(&mut peak);
                        }
                    }
                    for sid in &sessions {
                        let started = Instant::now();
                        kernel.inner.wake_history(sid).unwrap();
                        waits.push(started.elapsed().as_micros() as u64);
                    }
                } else {
                    let (mut w, mut p) = wake_all(&kernel, &sessions, &mut peak);
                    wakes.append(&mut w);
                    waits.append(&mut p);
                }
            }
        }
        let baseline_private = median(&mut resident_private);
        let baseline_working = median(&mut resident_working);
        let hibernated_private = median(&mut dormant_private);
        let hibernated_working = median(&mut dormant_working);
        let wake_p95 = p95(&mut wakes);
        let wait_p95 = p95(&mut waits);
        eprintln!(
            "[perf-ab] median private: {:.1} -> {:.1} MiB (down {:.1}); working: {:.1} -> {:.1} MiB (down {:.1})",
            mib(baseline_private),
            mib(hibernated_private),
            mib_difference(baseline_private, hibernated_private),
            mib(baseline_working),
            mib(hibernated_working),
            mib_difference(baseline_working, hibernated_working),
        );
        eprintln!("[perf-ab] wake_p95={}us resident_wait_p95={}us", wake_p95, wait_p95);
        let peak_ratio = peak.private_bytes as f64 / baseline_private.max(1) as f64;
        eprintln!(
            "[perf-ab] switch_peak_private={:.1}MiB baseline={:.1}MiB ratio={:.1}%",
            mib(peak.private_bytes),
            mib(baseline_private),
            peak_ratio * 100.
        );
        let pass = baseline_private.saturating_sub(hibernated_private) >= 30 * 1048576
            && baseline_working.saturating_sub(hibernated_working) >= 20 * 1048576
            && peak_ratio <= 1.10
            && wake_p95 <= 150_000
            && wait_p95 <= 20_000;
        eprintln!("[perf-ab] verdict={}", if pass { "pass" } else { "stop-before-default" });
        assert_eq!(kernel.inner.history_stats().resident_sessions, 50);
        assert_eq!(kernel.inner.history_stats().dormant_sessions, 0);
        kernel.shutdown();
        let deadline = Instant::now() + Duration::from_secs(10);
        while snapshots.exists()
            && std::fs::read_dir(&snapshots).is_ok_and(|mut entries| entries.next().is_some())
        {
            assert!(Instant::now() < deadline, "性能测试快照未清理");
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
