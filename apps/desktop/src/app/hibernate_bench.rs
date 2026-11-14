use super::*;
use serde_json::{Value, json};

impl ChatApp {
    pub(crate) fn hibernate_bench_command(
        &mut self,
        command: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        let action = command["action"].as_str().ok_or("缺少 action")?;
        let idx = command["token"].as_u64().and_then(|token| self.session_index(token));
        let mut result = Value::Null;
        match action {
            "status" => {}
            "open" => {
                let idx = self.open_session(KernelChoice::Builtin, self.cwd.clone());
                self.activate_session(idx, cx);
                result = json!({"token": self.sessions[idx].token});
            }
            "hibernate" => self.hibernate_idle_sessions(cx),
            "quit" => {
                for (_, slot) in &self.kernels {
                    if let Some(kernel) = &slot.kernel {
                        kernel.shutdown();
                    }
                }
            }
            _ => {
                let idx = idx.ok_or("找不到会话令牌")?;
                match action {
                    "activate" => self.activate_session(idx, cx),
                    "inspect" => {
                        use std::hash::{Hash, Hasher};
                        if !self.sessions[idx].history_loaded {
                            return Err("显示历史尚未就绪".into());
                        }
                        let s = &self.sessions[idx];
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        serde_json::to_string(
                            &json!({"transcript": &s.transcript, "subagents": &s.subagents}),
                        )
                        .unwrap()
                        .hash(&mut hasher);
                        result = json!({
                            "hash": hasher.finish().to_string(), "id": s.acp_id,
                            "mode": s.permission_mode, "thinking": s.thinking,
                            "users": s.user_message_count(),
                        });
                    }
                    "send" => {
                        let text = command["text"].as_str().ok_or("缺少 text")?.to_string();
                        let images = command
                            .get("images")
                            .cloned()
                            .map(serde_json::from_value)
                            .transpose()
                            .map_err(|err| err.to_string())?
                            .unwrap_or_default();
                        let sent_at_ms = bench_epoch_ms();
                        if !self.submit_to(
                            idx,
                            UserPrompt { text, images, attachments: Vec::new() },
                            cx,
                        ) {
                            return Err("发送被拒绝".into());
                        }
                        result = json!({"sent_at_ms": sent_at_ms});
                    }
                    "compact" => {
                        self.activate_session(idx, cx);
                        self.compact_session(cx);
                    }
                    "rewind" => {
                        self.activate_session(idx, cx);
                        self.hibernate_bench_rewind(window, cx)?;
                    }
                    "cancel" => {
                        self.activate_session(idx, cx);
                        self.stop(window, cx);
                    }
                    "close" => self.delete_session(idx, cx),
                    "answer" => {
                        self.activate_session(idx, cx);
                        if let Some(item) = self.sessions[idx].pending_permission() {
                            let Some(ChatItem::Permission { options, .. }) =
                                self.sessions[idx].transcript.get(item)
                            else {
                                return Err("权限行缺失".into());
                            };
                            let option = options
                                .iter()
                                .find(|option| option.kind == "allow_once")
                                .ok_or("没有单次允许选项")?;
                            let choice = Some((option.id.clone(), option.name.clone()));
                            self.resolve_permission(item, choice, cx);
                            result = json!("permission");
                        } else if let Some(item) = self.sessions[idx].pending_question() {
                            self.pick_question(item, 0, 0, cx);
                            result = json!("question");
                        } else {
                            return Err("会话没有待应答项".into());
                        }
                    }
                    _ => return Err(format!("未知动作 {action}")),
                }
            }
        }
        let sessions: Vec<_> = self.sessions.iter().map(|s| {
            let failures: Vec<_> = s.transcript.iter().filter_map(|item| match item {
                ChatItem::Failed { message, .. } => Some(message.to_string()),
                ChatItem::Tool(card) if card.phase == ToolPhase::Failed => Some(card.output.to_string()),
                ChatItem::Compaction { state: CompactState::Failed(message), .. } => Some(message.to_string()),
                _ => None,
            }).collect();
            json!({
                "token": s.token, "id": s.acp_id, "ready": s.ready, "busy": s.busy,
                "stopping": s.stopping, "rewinding": s.kernel_rewinding,
                "permission": s.waiting_permission(), "question": s.waiting_question(),
                "restored": s.restored, "mode": s.permission_mode,
                "history_loaded": s.history_loaded, "history_loading": s.history_loading,
                "history_error": s.history_error, "display_cached": s.display_snapshot.is_some(),
                "queued": s.queued.len(), "thinking": s.thinking,
                "users": s.user_message_count(),
                "agents": s.transcript.iter().filter(|item| matches!(item, ChatItem::Agent(_))).count(),
                "tools": s.transcript.iter().filter(|item| matches!(item, ChatItem::Tool(_))).count(),
                "subagents": s.subagents.len(),
                "compactions": s.transcript.iter().filter(|item| matches!(item, ChatItem::Compaction { state: CompactState::Done(_), .. })).count(),
                "display_bytes": s.retained_bytes(), "failures": failures,
            })
        }).collect();
        let stats = self
            .kernel_of(&KernelChoice::Builtin)
            .map(|kernel| kernel.history_stats())
            .unwrap_or_default();
        let tasks: Vec<_> = self.kernels.iter().flat_map(|(_, slot)| slot.tasks.iter()).collect();
        let snapshots: Vec<_> = claudesktop_builtin::default_hibernation_dir()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        Ok(json!({
            "result": result, "sessions": sessions, "tasks": tasks, "snapshots": snapshots,
            "display_loaded": self.sessions.iter().filter(|s| s.history_loaded && s.acp_id.is_some() && s.has_user_message()).count(),
            "display_bytes": self.sessions.iter().map(Session::retained_bytes).sum::<usize>(),
            "epoch_ms": bench_epoch_ms(),
            "resident": stats.resident_sessions, "dormant": stats.dormant_sessions,
            "transitioning": stats.transitioning_sessions, "history_bytes": stats.resident_bytes,
        }))
    }
}

pub(super) fn bench_epoch_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64() * 1000.
}

pub(super) fn record_event(session: &Option<String>, event: &KernelEvent) {
    let Some(dir) = std::env::var_os("CLAUDESKTOP_HIBERNATE_BENCH") else { return };
    if matches!(
        event,
        KernelEvent::AgentMessageChunk { .. }
            | KernelEvent::AgentThoughtChunk { .. }
            | KernelEvent::ToolCallProgress { .. }
            | KernelEvent::SubagentChunk { .. }
    ) {
        return;
    }
    use std::io::Write as _;
    let detail = match event {
        KernelEvent::Fatal(message)
        | KernelEvent::Notice(message)
        | KernelEvent::PromptFailed { message }
        | KernelEvent::CompactFailed { message }
        | KernelEvent::SessionFailed { message, .. } => Some(message.as_str()),
        _ => None,
    };
    let row = json!({"epoch_ms": bench_epoch_ms(), "session": session, "event": event.tag(), "detail": detail});
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(PathBuf::from(dir).join("events.jsonl"))
    {
        let _ = writeln!(file, "{row}");
    }
}
