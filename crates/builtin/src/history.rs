//! 用本地会话日志（[`Record`]）重建发给模型的对话历史。
//!
//! 日志只存了 UI 需要的内容：工具调用只有标题、状态和一行摘要，没有完整
//! 参数与输出。所以重建成纯文本的 user / assistant 交替消息，工具调用写成
//! `[tool] 标题 → 状态：摘要` 行并入 assistant 消息；系统提示词会告诉模型
//! 这些是摘要，需要细节就重新读。
//!
//! 日志里有压缩记录（`Compacted`）时，摘要另外返回（进系统提示词），记录之前的对话丢掉：
//! 一轮中间的自动压缩保留这一轮，空闲时的手动压缩全部丢掉（规则同 `compact` 模块）。

use crate::ChatMessage;
use crate::provider::ContentPart;
use claudesktop_core::{KernelEvent, Record};

/// 重建历史的总字符上限：超出时从最早的轮次开始丢
const HISTORY_MAX_CHARS: usize = 200_000;

enum Segment {
    Text(String),
    Tool { id: String, title: String, status: String, output: String },
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
            Segment::Tool { id: i, title, status, output } if i == id => {
                Some((title, status, output))
            }
            _ => None,
        })
    }

    /// 渲染成一条 assistant 消息；什么都没有返回 None。
    fn take(&mut self) -> Option<ChatMessage> {
        let mut out = String::new();
        for seg in self.segments.drain(..) {
            match seg {
                Segment::Text(t) => out.push_str(&t),
                Segment::Tool { title, status, output, .. } => {
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

/// 从日志重建的上下文。
#[derive(Debug, Default)]
pub struct Rebuilt {
    /// 最近一次压缩的摘要
    pub summary: Option<String>,
    /// 对话历史（不含系统提示词和摘要）
    pub messages: Vec<ChatMessage>,
}

/// 日志 → 对话历史。
pub fn rebuild(records: &[Record]) -> Rebuilt {
    // 回退过的轮次不进上下文
    let records = claudesktop_core::effective(records);
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut summary = None;
    let mut assistant = Assistant::default();
    // 有进行中的一轮（最后一条用户消息之后还没结束）
    let mut in_turn = false;
    for record in &records {
        match record {
            Record::User { text, images } => {
                messages.extend(assistant.take());
                messages.push(user_message(text, images));
                in_turn = true;
            }
            Record::Event { event } => match event {
                KernelEvent::AgentMessageChunk { text } => assistant.text(text),
                KernelEvent::ToolCallStarted { id, title, status, .. } => {
                    assistant.segments.push(Segment::Tool {
                        id: id.clone(),
                        title: title.clone(),
                        status: status.clone(),
                        output: String::new(),
                    })
                }
                KernelEvent::ToolCallUpdated {
                    id,
                    title: new_title,
                    status: new_status,
                    raw_output,
                    ..
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
                KernelEvent::TurnEnded { stop_reason } => {
                    if stop_reason == "cancelled" {
                        assistant.text("\n[interrupted by the user]");
                    }
                    in_turn = false;
                }
                KernelEvent::PromptFailed { message } => {
                    assistant.text(&format!("\n[turn failed: {message}]"));
                    in_turn = false;
                }
                // 待办清单写成一行工具摘要，续接后模型还知道做到哪了
                KernelEvent::PlanReplaced { entries } => assistant.segments.push(Segment::Tool {
                    id: String::new(),
                    title: crate::tools::TODO_WRITE.into(),
                    status: "completed".into(),
                    output: crate::tools::todo_summary(entries),
                }),
                KernelEvent::Compacted { summary: text, .. } => {
                    summary = Some(text.clone());
                    if in_turn {
                        // 本轮的用户消息起保留（本轮的回复还在 assistant 缓冲里）
                        let turn = messages.iter().rposition(|m| m.role == "user").unwrap_or(0);
                        messages.drain(..turn);
                    } else {
                        assistant.take();
                        messages.clear();
                    }
                }
                // 思考、权限请求不进上下文
                _ => {}
            },
            Record::PermissionResolved { .. }
            | Record::Turn { .. }
            | Record::Thought { .. }
            | Record::FileChanged { .. }
            | Record::Rewind { .. } => {}
        }
    }
    messages.extend(assistant.take());
    Rebuilt { summary, messages: trim(messages) }
}

/// 超出上限时从最早的轮次丢，并保证从一条用户消息开始。
fn trim(mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let len = |m: &ChatMessage| m.text().as_deref().map_or(0, str::len);
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

/// 用户记录 → 消息：图片附件从磁盘读回 base64（文件丢了就跳过，对话还能续）。
fn user_message(text: &str, images: &[String]) -> ChatMessage {
    use base64::Engine as _;
    let engine = base64::engine::general_purpose::STANDARD;
    let mut parts = vec![ContentPart::Text(text.to_string())];
    for path in images {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        parts.push(ContentPart::Image { media_type: mime_type(path), data: engine.encode(bytes) });
    }
    ChatMessage::with_parts("user", parts)
}

/// 附件扩展名 → MIME 类型（存的时候就是按扩展名写的）。
fn mime_type(path: &str) -> String {
    match path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    }
    .to_string()
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
            Record::User { text: "读一下 notes".into(), images: Vec::new() },
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
                output: None,
            }),
            ev(KernelEvent::AgentMessageChunk { text: "秘密是 42。".into() }),
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            Record::User { text: "再写进文件".into(), images: Vec::new() },
            ev(KernelEvent::ToolCallStarted {
                id: "c2".into(),
                title: "Write a.txt".into(),
                status: "pending".into(),
                kind: "edit".into(),
            }),
            ev(KernelEvent::TurnEnded { stop_reason: "cancelled".into() }),
            Record::User { text: "算了".into(), images: Vec::new() },
        ];
        let msgs = rebuild(&records).messages;
        let got: Vec<_> =
            msgs.iter().map(|m| format!("{}:{}", m.role, m.text().unwrap())).collect();
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
    fn compaction_keeps_the_running_turn_or_drops_everything() {
        let user = |t: &str| Record::User { text: t.into(), images: Vec::new() };
        let reply = |t: &str| ev(KernelEvent::AgentMessageChunk { text: t.into() });
        let end = || ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() });
        let compacted =
            |s: &str| ev(KernelEvent::Compacted { summary: s.into(), context_tokens: 0 });
        let texts = |r: &Rebuilt| r.messages.iter().map(|m| m.text().unwrap()).collect::<Vec<_>>();

        // 一轮中间的自动压缩：保留这一轮（含压缩前已有的回复）
        let r = rebuild(&[
            user("一"),
            reply("a"),
            end(),
            user("二"),
            reply("想"),
            compacted("摘要1"),
            reply("好"),
            end(),
        ]);
        assert_eq!(r.summary.as_deref(), Some("摘要1"));
        assert_eq!(texts(&r), ["二", "想好"]);

        // 空闲时的手动压缩：全丢，之后的轮次照常
        let r = rebuild(&[
            user("一"),
            reply("a"),
            end(),
            ev(KernelEvent::Compacting),
            compacted("摘要2"),
            end(),
            user("三"),
            reply("c"),
            end(),
        ]);
        assert_eq!(r.summary.as_deref(), Some("摘要2"));
        assert_eq!(texts(&r), ["三", "c"]);
    }

    #[test]
    fn trims_oldest_turns_and_starts_with_user() {
        let big = "x".repeat(HISTORY_MAX_CHARS / 2 + 10);
        let records = vec![
            Record::User { text: big.clone(), images: Vec::new() },
            ev(KernelEvent::AgentMessageChunk { text: big.clone() }),
            Record::User { text: "新问题".into(), images: Vec::new() },
            ev(KernelEvent::AgentMessageChunk { text: "新回答".into() }),
        ];
        let msgs = rebuild(&records).messages;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text().as_deref(), Some("新问题"));
    }
}
