//! 用本地会话日志（[`Record`]）重建发给模型的对话历史。
//!
//! 日志只存了 UI 需要的内容：工具调用只有标题、状态和一行摘要，没有完整
//! 参数与输出。所以重建成纯文本的 user / assistant 交替消息，工具调用写成
//! `[tool] 标题 → 状态：摘要` 行并入 assistant 消息；系统提示词会告诉模型
//! 这些是摘要，需要细节就重新读。

use crate::ChatMessage;
use claudesktop_core::{KernelEvent, Record};

/// 重建历史的总字符上限：超出时从最早的轮次开始丢
const HISTORY_MAX_CHARS: usize = 200_000;

enum Segment {
    Text(String),
    Tool {
        id: String,
        title: String,
        status: String,
        output: String,
    },
}

#[derive(Default)]
struct Assistant {
    segments: Vec<Segment>,
}

impl Assistant {
    fn text(&mut self, t: &str) {
        match self.segments.last_mut() {
            Some(Segment::Text(buf)) => buf.push_str(t),
            _ => self.segments.push(Segment::Text(t.to_string())),
        }
    }

    fn tool(&mut self, id: &str) -> Option<(&mut String, &mut String, &mut String)> {
        self.segments.iter_mut().rev().find_map(|s| match s {
            Segment::Tool {
                id: i,
                title,
                status,
                output,
            } if i == id => Some((title, status, output)),
            _ => None,
        })
    }

    /// 渲染成一条 assistant 消息；什么都没有返回 None。
    fn take(&mut self) -> Option<ChatMessage> {
        let mut out = String::new();
        for seg in self.segments.drain(..) {
            match seg {
                Segment::Text(t) => out.push_str(&t),
                Segment::Tool {
                    title,
                    status,
                    output,
                    ..
                } => {
                    // 没等到结果的调用（上次退出时还在进行 / 等权限）
                    let (status, output) = match status.as_str() {
                        "completed" | "failed" => (status, output),
                        _ => ("interrupted".to_string(), String::new()),
                    };
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&format!("[tool] {title} → {status}"));
                    if !output.is_empty() {
                        out.push_str(&format!("：{output}"));
                    }
                    out.push('\n');
                }
            }
        }
        let out = out.trim().to_string();
        (!out.is_empty()).then(|| ChatMessage::new("assistant", out))
    }
}

/// 日志 → 对话历史（不含系统提示词）。
pub fn rebuild(records: &[Record]) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    let mut assistant = Assistant::default();
    for record in records {
        match record {
            Record::User { text } => {
                messages.extend(assistant.take());
                messages.push(ChatMessage::new("user", text.clone()));
            }
            Record::Event { event } => match event {
                KernelEvent::AgentMessageChunk { text } => assistant.text(text),
                KernelEvent::ToolCallStarted {
                    id, title, status, ..
                } => assistant.segments.push(Segment::Tool {
                    id: id.clone(),
                    title: title.clone(),
                    status: status.clone(),
                    output: String::new(),
                }),
                KernelEvent::ToolCallUpdated {
                    id,
                    title: new_title,
                    status: new_status,
                    raw_output,
                } => {
                    if let Some((title, status, output)) = assistant.tool(id) {
                        if let Some(t) = new_title {
                            *title = t.clone();
                        }
                        if let Some(s) = new_status {
                            *status = s.clone();
                        }
                        if let Some(o) = raw_output {
                            *output = o.clone();
                        }
                    }
                }
                KernelEvent::TurnEnded { stop_reason } if stop_reason == "cancelled" => {
                    assistant.text("\n[interrupted by the user]")
                }
                KernelEvent::PromptFailed { message } => {
                    assistant.text(&format!("\n[turn failed: {message}]"))
                }
                // 思考、计划、权限请求不进上下文
                _ => {}
            },
            Record::PermissionResolved { .. } | Record::Turn { .. } => {}
        }
    }
    messages.extend(assistant.take());
    trim(messages)
}

/// 超出上限时从最早的轮次丢，并保证从一条用户消息开始。
fn trim(mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let len = |m: &ChatMessage| m.content.as_deref().map_or(0, str::len);
    let mut total: usize = messages.iter().map(len).sum();
    let mut start = 0;
    while total > HISTORY_MAX_CHARS && start < messages.len() {
        total -= len(&messages[start]);
        start += 1;
    }
    while start < messages.len() && messages[start].role != "user" {
        start += 1;
    }
    messages.drain(..start);
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: KernelEvent) -> Record {
        Record::Event { event }
    }

    #[test]
    fn rebuilds_turns_with_tool_summaries() {
        let records = vec![
            Record::User { text: "读一下 notes".into() },
            ev(KernelEvent::AgentThoughtChunk { text: "想想".into() }),
            ev(KernelEvent::AgentMessageChunk { text: "我先".into() }),
            ev(KernelEvent::AgentMessageChunk { text: "看看。".into() }),
            ev(KernelEvent::ToolCallStarted {
                id: "c1".into(),
                title: "Read notes.txt".into(),
                status: "pending".into(),
                kind: "read".into(),
            }),
            ev(KernelEvent::ToolCallUpdated {
                id: "c1".into(),
                title: None,
                status: Some("completed".into()),
                raw_output: Some("1 行".into()),
            }),
            ev(KernelEvent::AgentMessageChunk { text: "秘密是 42。".into() }),
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            Record::User { text: "再写进文件".into() },
            ev(KernelEvent::ToolCallStarted {
                id: "c2".into(),
                title: "Write a.txt".into(),
                status: "pending".into(),
                kind: "edit".into(),
            }),
            ev(KernelEvent::TurnEnded { stop_reason: "cancelled".into() }),
            Record::User { text: "算了".into() },
        ];
        let msgs = rebuild(&records);
        let got: Vec<_> = msgs
            .iter()
            .map(|m| format!("{}:{}", m.role, m.content.as_deref().unwrap()))
            .collect();
        assert_eq!(
            got,
            [
                "user:读一下 notes",
                "assistant:我先看看。\n[tool] Read notes.txt → completed：1 行\n秘密是 42。",
                "user:再写进文件",
                "assistant:[tool] Write a.txt → interrupted\n\n[interrupted by the user]",
                "user:算了",
            ]
        );
    }

    #[test]
    fn trims_oldest_turns_and_starts_with_user() {
        let big = "x".repeat(HISTORY_MAX_CHARS / 2 + 10);
        let records = vec![
            Record::User { text: big.clone() },
            ev(KernelEvent::AgentMessageChunk { text: big.clone() }),
            Record::User { text: "新问题".into() },
            ev(KernelEvent::AgentMessageChunk { text: "新回答".into() }),
        ];
        let msgs = rebuild(&records);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content.as_deref(), Some("新问题"));
    }
}
