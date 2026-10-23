//! 上下文压缩的纯逻辑：估算 token、挑切点、把要压缩的那段渲染成文字交给模型总结。
//!
//! 压缩后的上下文 = 系统提示词里的「之前对话的摘要」+ 保留的尾部。尾部总是从一条用户消息开始：
//! - 平常：保留本轮（最后一条用户消息起），之前的轮次进摘要；
//! - 本轮自己就太长（工具步骤很多）：保留本轮的用户消息 + 最近几步，中间的步骤进摘要；
//! - 空闲时手动压缩：全部进摘要，尾部为空。
//!
//! 从日志重建时（`history::rebuild`）按同样的规则处理压缩记录：进行中的一轮整轮保留。

use crate::provider::{ChatMessage, ContentPart};

/// 用量达到窗口的这个比例时，下一次请求前自动压缩
pub const AUTO_COMPACT_PERCENT: u64 = 80;
/// 压缩后保留的尾部最多占窗口的比例
pub const KEEP_PERCENT: u64 = 25;
/// 交给模型总结的文字最多占窗口的比例（留出系统提示词和输出的余量）
pub const TRANSCRIPT_PERCENT: u64 = 60;
/// 窗口未知时，交给模型总结的文字的上限（字节）
pub const TRANSCRIPT_MAX_BYTES_UNKNOWN: usize = 400_000;
/// 一张图片按这么多 token 估
const IMAGE_TOKENS: u64 = 1_500;
/// 渲染时工具参数 / 工具结果各自最多保留的字符数
const TOOL_ARGS_CHARS: usize = 400;
const TOOL_RESULT_CHARS: usize = 1_500;

/// 粗估一组消息的 token：ASCII 按字节 / 4，非 ASCII（中文等）按每字符 1.5——
/// 旧算法一个中文字 3 字节只算 0.75 token，实际普遍在 1~2 之间，触发压缩会偏晚。
pub fn estimate_tokens(messages: &[ChatMessage]) -> u64 {
    messages.iter().map(estimate_message).sum()
}

/// 一段文字的 token：ASCII 4 字符 ≈ 1 token，非 ASCII 每字符按 1.5 记。
fn estimate_text(text: &str) -> u64 {
    let mut ascii = 0u64;
    let mut wide = 0u64;
    for c in text.chars() {
        if c.is_ascii() {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    // wide × 1.5 = wide × 6 / 4，并进同一趟除法
    (ascii + wide * 6) / 4
}

fn estimate_message(m: &ChatMessage) -> u64 {
    let mut tokens = 0u64;
    let mut images = 0u64;
    for part in &m.parts {
        match part {
            ContentPart::Text(t) => tokens += estimate_text(t),
            ContentPart::Image { .. } => images += 1,
        }
    }
    for call in &m.tool_calls {
        tokens += estimate_text(&call.function.name) + estimate_text(&call.function.arguments);
    }
    tokens + images * IMAGE_TOKENS + 4
}

/// 压缩方案：`history[..tail_from]` 进摘要（`keep_user` 那条除外），其余保留。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split {
    /// 保留的尾部从这里开始
    pub tail_from: usize,
    /// 本轮内切开时，额外保留的本轮用户消息下标（放在尾部最前面）
    pub keep_user: Option<usize>,
}

impl Split {
    /// 压缩后的历史（不含摘要，摘要进系统提示词）。
    pub fn apply(&self, history: &[ChatMessage]) -> Vec<ChatMessage> {
        let mut out: Vec<ChatMessage> =
            self.keep_user.map(|i| history[i].clone()).into_iter().collect();
        out.extend(history[self.tail_from..].iter().cloned());
        out
    }
}

/// 挑切点。`idle` = 没有进行中的一轮（手动压缩），全部进摘要。
/// `keep_budget` 是保留尾部的 token 预算。没有可压缩的内容返回 None。
pub fn split(history: &[ChatMessage], keep_budget: u64, idle: bool) -> Option<Split> {
    if idle {
        return (!history.is_empty())
            .then_some(Split { tail_from: history.len(), keep_user: None });
    }
    let turn = history.iter().rposition(ChatMessage::is_user_turn)?;
    if turn > 0 && estimate_tokens(&history[turn..]) <= keep_budget {
        return Some(Split { tail_from: turn, keep_user: None });
    }
    // 本轮本身就超预算：在本轮的步骤之间切（每步从一条 assistant 消息开始）
    let user = estimate_message(&history[turn]);
    let steps: Vec<usize> =
        (turn + 1..history.len()).filter(|&i| history[i].role == "assistant").collect();
    let fits = steps
        .iter()
        .copied()
        .find(|&i| user + estimate_tokens(&history[i..]) <= keep_budget)
        .or_else(|| steps.last().copied());
    match fits {
        // 第一步就放得下 = 本轮没什么可压的，退回按轮切
        Some(i) if i > turn + 1 => Some(Split { tail_from: i, keep_user: Some(turn) }),
        _ if turn > 0 => Some(Split { tail_from: turn, keep_user: None }),
        _ => None,
    }
}

/// 截到 `max` 个字符，截掉了就注明剩多少。
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{} …[{} more chars]", &text[..cut], text[cut..].chars().count()),
        None => text.to_string(),
    }
}

/// 要压缩的那段 → 给模型看的文字记录。工具参数和结果截短；超过 `max_bytes` 时丢掉最早的部分。
pub fn transcript(head: &[ChatMessage], max_bytes: usize) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for m in head {
        let text = m.text().unwrap_or_default();
        let images = m.parts.iter().filter(|p| matches!(p, ContentPart::Image { .. })).count();
        let mut block = match m.role.as_str() {
            "user" => format!("### User\n{text}"),
            "tool" => format!("[tool result] {}", clip(text.trim(), TOOL_RESULT_CHARS)),
            _ => format!("### Assistant\n{text}"),
        };
        if images > 0 {
            block.push_str(&format!("\n[{images} image(s) attached]"));
        }
        for call in &m.tool_calls {
            block.push_str(&format!(
                "\n[tool call] {} {}",
                call.function.name,
                clip(&call.function.arguments, TOOL_ARGS_CHARS)
            ));
        }
        blocks.push(block.trim_end().to_string());
    }
    let mut total: usize = blocks.iter().map(|b| b.len() + 2).sum();
    let mut start = 0;
    while total > max_bytes && start + 1 < blocks.len() {
        total -= blocks[start].len() + 2;
        start += 1;
    }
    let mut out = String::new();
    if start > 0 {
        out.push_str("[the earliest part of the conversation was omitted for length]\n\n");
    }
    out.push_str(&blocks[start..].join("\n\n"));
    out
}

const SUMMARY_SYSTEM: &str = "You compress the history of a coding-agent session so the work can continue \
in a fresh context window. You only write the summary; you cannot call tools.";

const SUMMARY_INSTRUCTIONS: &str = "Summarize the conversation above so that the agent can continue \
seamlessly without it. Be concise but keep everything needed to continue:\n\
1. The user's goals and requests, with their exact wording for important instructions and constraints.\n\
2. Decisions made and approaches rejected (and why).\n\
3. Files read, created or modified, with the key facts about each (paths, functions, what changed).\n\
4. Commands run and their important results (errors, test outcomes).\n\
5. The current state of the work and what remains to be done, including the immediate next step.\n\
Write in the language the user writes in. Use Markdown headings and bullet lists. \
Do not add a preamble or closing remarks.";

/// 总结请求：系统提示词 + 一条用户消息（之前的摘要 + 这段记录 + 要求）。
/// `hook_context` = `PreCompact` 钩子的补充说明（如「别忘了 X」），插在要求前。
pub fn summary_request(
    previous: Option<&str>,
    transcript: &str,
    hook_context: &[String],
) -> Vec<ChatMessage> {
    let mut body = String::new();
    if let Some(prev) = previous {
        body.push_str(&format!(
            "<earlier-summary>\nA summary of the conversation before the part below:\n\n{prev}\n</earlier-summary>\n\n"
        ));
    }
    body.push_str(&format!("<conversation>\n{transcript}\n</conversation>\n\n"));
    if !hook_context.is_empty() {
        body.push_str(&format!(
            "<plugin-hook-context>\n{}\n</plugin-hook-context>\n\n",
            hook_context.join("\n\n")
        ));
    }
    body.push_str(SUMMARY_INSTRUCTIONS);
    vec![ChatMessage::new("system", SUMMARY_SYSTEM), ChatMessage::new("user", body)]
}

/// 系统提示词里的摘要一节。
pub fn prompt_section(summary: &str) -> String {
    format!(
        "# Summary of the earlier conversation\n\
         The earlier part of this conversation was compacted to save context. \
         This summary replaces it; re-read files if you need details that are not here.\n\n{summary}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{FunctionCall, ToolCall};

    fn user(t: &str) -> ChatMessage {
        ChatMessage::new("user", t)
    }
    fn reply(t: &str) -> ChatMessage {
        ChatMessage::new("assistant", t)
    }
    fn call(id: &str, args: &str) -> ChatMessage {
        ChatMessage::assistant_tool_calls(
            String::new(),
            vec![ToolCall {
                id: id.into(),
                kind: "function".into(),
                function: FunctionCall { name: "read_file".into(), arguments: args.into() },
            }],
        )
    }
    fn result(id: &str, t: &str) -> ChatMessage {
        ChatMessage::tool_result(id.into(), t.into())
    }

    #[test]
    fn keeps_the_current_turn_when_it_fits() {
        let h = vec![user("一"), reply("a"), user("二"), reply("b"), user("三")];
        let s = split(&h, 1_000, false).unwrap();
        assert_eq!(s, Split { tail_from: 4, keep_user: None });
        let kept = s.apply(&h);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text().unwrap(), "三");
        // 只有一轮、而且放得下：没什么可压
        assert_eq!(split(&[user("只有一轮")], 1_000, false), None);
    }

    #[test]
    fn cuts_between_steps_when_the_turn_is_too_long() {
        let big = "x".repeat(4_000); // ≈ 1000 token
        let h = vec![
            user("改一下"),
            call("c1", "{}"),
            result("c1", &big),
            call("c2", "{}"),
            result("c2", &big),
            call("c3", "{}"),
            result("c3", "短"),
        ];
        let s = split(&h, 500, false).unwrap();
        assert_eq!(s, Split { tail_from: 5, keep_user: Some(0) });
        let kept = s.apply(&h);
        let roles: Vec<&str> = kept.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "tool"]);
        // 最近一步都放不下：也只留最近一步
        let s = split(&h[..5], 100, false).unwrap();
        assert_eq!(s, Split { tail_from: 3, keep_user: Some(0) });
    }

    /// 代工具发图片的 user 消息（OpenAI 兼容协议）不是这一轮的开头：切点和保留的用户消息
    /// 都要落在真正的用户消息上
    #[test]
    fn tool_image_messages_do_not_start_a_turn() {
        let big = "x".repeat(4_000);
        let mut images = user("Image(s) returned by the tool calls above:");
        images.tool_images = true;
        let h = vec![
            user("看看截图"),
            call("c1", "{}"),
            result("c1", &big),
            images,
            call("c2", "{}"),
            result("c2", &big),
            call("c3", "{}"),
            result("c3", "短"),
        ];
        let s = split(&h, 500, false).unwrap();
        assert_eq!(s, Split { tail_from: 6, keep_user: Some(0) });
        assert_eq!(s.apply(&h)[0].text().unwrap(), "看看截图");
    }

    #[test]
    fn idle_compacts_everything() {
        let h = vec![user("一"), reply("a")];
        let s = split(&h, 0, true).unwrap();
        assert!(s.apply(&h).is_empty());
        assert_eq!(split(&[], 0, true), None);
    }

    #[test]
    fn transcript_clips_tool_output_and_drops_oldest() {
        let h = vec![
            user("第一轮"),
            call("c1", r#"{"path":"a.rs"}"#),
            result("c1", &"y".repeat(5_000)),
            reply("完成"),
        ];
        let t = transcript(&h, 100_000);
        assert!(t.starts_with("### User\n第一轮"), "{t}");
        assert!(t.contains("[tool call] read_file {\"path\":\"a.rs\"}"), "{t}");
        assert!(t.contains("more chars]"), "{t}");
        let short = transcript(&h, 50);
        assert!(short.starts_with("[the earliest part"), "{short}");
        assert!(short.ends_with("### Assistant\n完成"), "{short}");
    }

    #[test]
    fn summary_request_carries_the_previous_summary() {
        let req = summary_request(Some("旧摘要"), "记录", &[]);
        assert_eq!(req[0].role, "system");
        let body = req[1].text().unwrap();
        assert!(body.contains("旧摘要") && body.contains("<conversation>\n记录"), "{body}");
        assert!(!summary_request(None, "记录", &[])[1].text().unwrap().contains("earlier-summary"));
    }

    #[test]
    fn summary_request_carries_hook_context() {
        let body =
            summary_request(None, "记录", &["别忘了构建脚本".into()])[1].text().unwrap().to_owned();
        assert!(
            body.contains("<plugin-hook-context>\n别忘了构建脚本\n</plugin-hook-context>"),
            "{body}"
        );
        assert!(body.ends_with("Do not add a preamble or closing remarks."), "{body}");
    }

    #[test]
    fn estimates_by_bytes() {
        assert_eq!(estimate_tokens(&[user(&"a".repeat(400))]), 104);
    }

    #[test]
    fn chinese_counts_more_than_bytes_div_4() {
        // 400 个汉字：旧算法 300 字节 / 4 = 75 + 4；现在按每字 1.5 ≈ 604
        let tokens = estimate_tokens(&[user(&"中".repeat(400))]);
        assert_eq!(tokens, 604);
        // 混合文本在两档之间
        let mixed = estimate_tokens(&[user(&("a".repeat(200) + &"中".repeat(100)))]);
        assert_eq!(mixed, 50 + 150 + 4);
    }
}
