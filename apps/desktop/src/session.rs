//! 单个对话会话的状态，以及把内核事件 / 演示步骤应用到对话记录上。

use crate::demo::*;
use crate::model::*;
use claudesktop_core::{Kernel, KernelEvent, Record, SessionLog, SessionMeta};
use gpui_kit::*;
use std::path::PathBuf;
use std::sync::Arc;

/// 一个对话会话：自己的对话记录、忙闲、模式、模型。一条内核连接上可以同时有多个。
pub struct Session {
    pub title: SharedString,
    /// 创建时的令牌，用来把内核回的 Started / SessionFailed 对回这个会话
    pub token: u64,
    /// ACP sessionId（Started 之后才有；演示回放模式为 None）
    pub acp_id: Option<String>,
    /// 已向内核发出 session/new
    pub requested: bool,
    /// 可以发提示词：Started 已到，或演示回放模式
    pub ready: bool,
    pub transcript: Vec<ChatItem>,
    pub busy: bool,
    /// 演示回放用：递增即取消正在播放的脚本
    pub cancel_token: u64,
    pub scroll: ScrollHandle,
    pub follow_scroll: bool,
    /// 当前可选模式（默认静态表，Started 后换成 agent 上报的）
    pub modes: Vec<ModeEntry>,
    pub permission_mode: usize, // modes 下标
    pub model_label: SharedString,
    /// 自动演示：会话就绪后发送的提示词
    pub autoprompt: Option<String>,
    /// 会话工作目录（侧栏按它分组；session/new 用它）
    pub cwd: PathBuf,
    /// 持久化元信息：首条消息发出后才建档（空会话不落盘）
    pub meta: Option<SessionMeta>,
    /// 追加式日志；恢复的会话在第一次写入时才打开
    pub log: Option<SessionLog>,
    /// 从磁盘恢复、还没接上内核会话的历史会话（不自动 session/new）
    pub restored: bool,
}

impl Session {
    pub fn new(token: u64, ready: bool, cwd: PathBuf) -> Self {
        Self {
            title: "新会话".into(),
            token,
            acp_id: None,
            requested: false,
            ready,
            transcript: Vec::new(),
            busy: false,
            cancel_token: 0,
            scroll: ScrollHandle::new(),
            follow_scroll: true,
            modes: default_modes(),
            permission_mode: 0,
            model_label: "GLM-5.3".into(),
            autoprompt: None,
            cwd,
            meta: None,
            log: None,
            restored: false,
        }
    }

    /// 从持久化记录重放出一个历史会话：事件走与实时完全相同的映射。
    pub fn from_records(token: u64, meta: SessionMeta, records: Vec<Record>) -> Self {
        let mut s = Self::new(token, false, meta.cwd.clone());
        s.title = meta.title.clone().into();
        s.restored = true;
        for record in records {
            match record {
                Record::User { text } => s.transcript.push(ChatItem::User(text.into())),
                Record::Event { event } => {
                    s.apply_event(event, None);
                }
                Record::PermissionResolved { id, label } => {
                    let item = s.transcript.iter_mut().rev().find(|i| {
                        matches!(i, ChatItem::Permission { id: pid, resolved: None, .. } if *pid == id)
                    });
                    if let Some(ChatItem::Permission { resolved, .. }) = item {
                        *resolved = Some(label.into());
                    }
                }
            }
        }
        // 上次退出时还在进行的东西一律收尾
        s.finish_thinking();
        for item in &mut s.transcript {
            if let ChatItem::Permission { resolved: r @ None, .. } = item {
                *r = Some("已取消".into());
            }
        }
        s.busy = false;
        s.meta = Some(meta);
        s
    }

    pub fn has_user_message(&self) -> bool {
        self.transcript
            .iter()
            .any(|i| matches!(i, ChatItem::User(_)))
    }

    pub fn note(&mut self, text: impl Into<SharedString>) {
        self.transcript.push(ChatItem::Note(text.into()));
    }

    pub fn finish_thinking(&mut self) {
        if let Some(ChatItem::Thinking { streaming, open, .. }) = self.transcript.last_mut() {
            *streaming = false;
            *open = false;
        }
    }

    /// 应答某条权限请求：`Some((选项 ID, 展示名))` 选择，`None` 取消。
    pub fn resolve_permission(
        &mut self,
        idx: usize,
        choice: Option<(String, String)>,
        kernel: Option<&Arc<dyn Kernel>>,
    ) {
        let Some(kernel) = kernel else {
            return;
        };
        if let Some(ChatItem::Permission { id, resolved, .. }) = self.transcript.get_mut(idx) {
            if resolved.is_some() {
                return;
            }
            let label: SharedString = match &choice {
                Some((_, name)) => name.clone().into(),
                None => "已取消".into(),
            };
            *resolved = Some(match kernel.resolve_permission(*id, choice.map(|(i, _)| i)) {
                Ok(()) => label,
                Err(err) => format!("应答失败：{err}").into(),
            });
        }
    }

    /// 本轮结束 / 停止 / 内核出错时，仍在等待的请求一律按「取消」收尾。
    pub fn cancel_pending_permissions(&mut self, kernel: Option<&Arc<dyn Kernel>>) {
        let Some(kernel) = kernel else {
            return;
        };
        for item in &mut self.transcript {
            if let ChatItem::Permission { id, resolved, .. } = item {
                if resolved.is_none() {
                    let _ = kernel.resolve_permission(*id, None);
                    *resolved = Some("已取消".into());
                }
            }
        }
    }

    /// 内核致命错误：这个会话不再可用。
    pub fn on_fatal(&mut self, message: &str, kernel: Option<&Arc<dyn Kernel>>) {
        self.busy = false;
        self.ready = false;
        self.cancel_pending_permissions(kernel);
        self.note(format!("内核错误：{message}"));
    }

    /// 内核事件 → 本会话的界面内容（ACP `session/update` 的 UI 投影）。
    /// 返回值：会话刚就绪且有待发的自动提示词时，交给调用方发送。
    pub fn apply_event(
        &mut self,
        event: KernelEvent,
        kernel: Option<&Arc<dyn Kernel>>,
    ) -> Option<String> {
        match event {
            KernelEvent::Started {
                session_id,
                model,
                modes,
                current_mode,
                ..
            } => {
                self.acp_id = Some(session_id);
                self.ready = true;
                if let Some(model) = model {
                    // provider/family/name → 只留最后一段，顶栏空间有限
                    self.model_label =
                        model.rsplit('/').next().unwrap_or(&model).to_string().into();
                }
                if !modes.is_empty() {
                    self.modes = modes
                        .into_iter()
                        .map(|m| ModeEntry {
                            icon: mode_icon(&m.id),
                            desc: m.description.unwrap_or_default().into(),
                            name: m.name.into(),
                            id: m.id,
                        })
                        .collect();
                    self.permission_mode = current_mode
                        .and_then(|id| self.modes.iter().position(|m| m.id == id))
                        .unwrap_or(0);
                }
                return self.autoprompt.take();
            }
            KernelEvent::AgentMessageChunk { text } => {
                self.finish_thinking();
                // 首条提示之前到达的文本是 agent 的启动信息，折叠显示
                let before_first_prompt = !self.has_user_message();
                match self.transcript.last_mut() {
                    Some(ChatItem::Info { text: buf, .. }) if before_first_prompt => {
                        buf.push_str(&text)
                    }
                    Some(ChatItem::Agent(buf)) => buf.push_str(&text),
                    _ if before_first_prompt => self.transcript.push(ChatItem::Info {
                        text,
                        open: false,
                    }),
                    _ => self.transcript.push(ChatItem::Agent(text)),
                }
            }
            KernelEvent::AgentThoughtChunk { text } => match self.transcript.last_mut() {
                Some(ChatItem::Thinking {
                    text: buf,
                    streaming: true,
                    ..
                }) => buf.push_str(&text),
                _ => self.transcript.push(ChatItem::Thinking {
                    text,
                    streaming: true,
                    open: true,
                }),
            },
            KernelEvent::ToolCallStarted {
                id,
                title,
                status,
                kind,
            } => {
                self.finish_thinking();
                let (name, arg) = tool_label(&kind, &title);
                self.transcript.push(ChatItem::Tool(ToolCard {
                    id: Some(id),
                    kind,
                    name,
                    arg,
                    result: SharedString::default(),
                    phase: tool_phase(&status),
                }));
            }
            KernelEvent::ToolCallUpdated {
                id,
                title,
                status,
                raw_output,
            } => {
                let card = self.transcript.iter_mut().rev().find_map(|item| match item {
                    ChatItem::Tool(card) if card.id.as_deref() == Some(id.as_str()) => Some(card),
                    _ => None,
                });
                if let Some(card) = card {
                    if let Some(title) = title {
                        let (name, arg) = tool_label(&card.kind, &title);
                        card.name = name;
                        card.arg = arg;
                    }
                    if let Some(status) = status {
                        card.phase = tool_phase(&status);
                    }
                    if let Some(raw) = raw_output {
                        card.result = summarize_output(&raw).into();
                    }
                }
            }
            KernelEvent::PlanReplaced { entries } => {
                self.finish_thinking();
                let rows: Vec<PlanRow> = entries
                    .into_iter()
                    .map(|e| PlanRow {
                        done: e.status == "completed",
                        text: e.content.into(),
                    })
                    .collect();
                let turn_start = self
                    .transcript
                    .iter()
                    .rposition(|i| matches!(i, ChatItem::User(_)))
                    .map_or(0, |i| i + 1);
                let existing = self.transcript[turn_start..]
                    .iter()
                    .rposition(|i| matches!(i, ChatItem::Plan(_)))
                    .map(|i| i + turn_start);
                match existing {
                    Some(i) => self.transcript[i] = ChatItem::Plan(rows),
                    None => self.transcript.push(ChatItem::Plan(rows)),
                }
            }
            KernelEvent::PermissionAuto { title, chosen } => {
                self.note(format!("已自动批准：{title} · {chosen}"));
            }
            KernelEvent::PermissionRequest { id, title, options } => {
                self.finish_thinking();
                self.transcript.push(ChatItem::Permission {
                    id,
                    title: title.into(),
                    options,
                    resolved: None,
                });
            }
            KernelEvent::PromptFailed { message } => {
                self.finish_thinking();
                self.busy = false;
                self.cancel_pending_permissions(kernel);
                self.note(format!("本轮失败：{message}"));
            }
            KernelEvent::TurnEnded { stop_reason } => {
                self.finish_thinking();
                self.busy = false;
                self.cancel_pending_permissions(kernel);
                if stop_reason != "end_turn" {
                    self.note(format!("本轮结束：{stop_reason}"));
                }
            }
            // 连接级事件由 ChatApp 处理；其余（状态、fs、回显）界面不展示
            KernelEvent::Connected { .. }
            | KernelEvent::SessionFailed { .. }
            | KernelEvent::Fatal(_)
            | KernelEvent::Status(_)
            | KernelEvent::UserMessageChunk { .. }
            | KernelEvent::FsRead { .. }
            | KernelEvent::FsWrite { .. } => {}
        }
        None
    }

    pub fn apply_mock(&mut self, action: &MockAction) {
        match action {
            MockAction::BeginThinking => {
                self.transcript.push(ChatItem::Thinking {
                    text: String::new(),
                    streaming: true,
                    open: true,
                });
            }
            MockAction::ThinkChunk(chunk) => {
                if let Some(ChatItem::Thinking { text, .. }) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndThinking => {
                if let Some(ChatItem::Thinking { streaming, open, .. }) =
                    self.transcript.last_mut()
                {
                    *streaming = false;
                    *open = false;
                }
            }
            MockAction::BeginTool(name, arg) => {
                self.transcript.push(ChatItem::Tool(ToolCard {
                    id: None,
                    kind: String::new(),
                    name: (*name).into(),
                    arg: (*arg).into(),
                    result: SharedString::default(),
                    phase: ToolPhase::Running,
                }));
            }
            MockAction::ToolDone(result) => {
                if let Some(ChatItem::Tool(card)) = self.transcript.last_mut() {
                    card.result = (*result).into();
                    card.phase = ToolPhase::Done;
                }
            }
            MockAction::Diff(path, rows) => {
                self.transcript.push(ChatItem::Diff { path, rows });
            }
            MockAction::BeginPlan => {
                self.transcript.push(ChatItem::Plan(Vec::new()));
            }
            MockAction::PlanRowAdd(row) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    rows.push(PlanRow {
                        text: (*row).into(),
                        done: false,
                    });
                }
            }
            MockAction::PlanRowDone(i) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    if let Some(row) = rows.get_mut(*i) {
                        row.done = true;
                    }
                }
            }
            MockAction::BeginAgent => {
                self.transcript.push(ChatItem::Agent(String::new()));
            }
            MockAction::AgentChunk(chunk) => {
                if let Some(ChatItem::Agent(text)) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndAgent => {}
            MockAction::Note(text) => {
                self.transcript.push(ChatItem::Note((*text).into()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claudesktop_core::PermissionOptionView;

    fn meta() -> SessionMeta {
        SessionMeta {
            id: "s1".into(),
            title: "读 README".into(),
            cwd: "C:/proj".into(),
            agent: "mock".into(),
            kernel_session_id: Some("acp-1".into()),
            created_ms: 0,
            updated_ms: 0,
        }
    }

    fn ev(event: KernelEvent) -> Record {
        Record::Event { event }
    }

    fn permission(id: u64) -> Record {
        ev(KernelEvent::PermissionRequest {
            id,
            title: "Write a.txt".into(),
            options: vec![PermissionOptionView {
                id: "allow".into(),
                name: "Yes".into(),
                kind: "allow_once".into(),
            }],
        })
    }

    #[core::prelude::v1::test]
    fn replay_rebuilds_transcript() {
        let records = vec![
            Record::User { text: "读 README".into() },
            ev(KernelEvent::AgentThoughtChunk { text: "想一想".into() }),
            ev(KernelEvent::ToolCallStarted {
                id: "t1".into(),
                title: "Read README.md".into(),
                status: "in_progress".into(),
                kind: "read".into(),
            }),
            ev(KernelEvent::ToolCallUpdated {
                id: "t1".into(),
                title: None,
                status: Some("completed".into()),
                raw_output: Some("# claudesktop".into()),
            }),
            permission(1),
            Record::PermissionResolved { id: 1, label: "Yes".into() },
            ev(KernelEvent::AgentMessageChunk { text: "总结完毕".into() }),
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            // 上次退出时没应答的请求
            Record::User { text: "再写个文件".into() },
            permission(2),
        ];
        let s = Session::from_records(7, meta(), records);

        assert!(s.restored && !s.ready && !s.busy);
        assert_eq!(s.title.as_ref(), "读 README");
        assert_eq!(s.cwd, PathBuf::from("C:/proj"));
        let kinds: Vec<&str> = s
            .transcript
            .iter()
            .map(|i| match i {
                ChatItem::User(_) => "user",
                ChatItem::Thinking { .. } => "thinking",
                ChatItem::Tool(_) => "tool",
                ChatItem::Permission { .. } => "permission",
                ChatItem::Agent(_) => "agent",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            ["user", "thinking", "tool", "permission", "agent", "user", "permission"]
        );
        assert!(matches!(&s.transcript[1], ChatItem::Thinking { streaming: false, .. }));
        assert!(matches!(&s.transcript[2], ChatItem::Tool(c) if c.phase == ToolPhase::Done));
        let labels: Vec<_> = s
            .transcript
            .iter()
            .filter_map(|i| match i {
                ChatItem::Permission { resolved, .. } => resolved.as_ref().map(|r| r.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["Yes", "已取消"]);
    }
}
