//! 输入框的下文补全：一个很小的独立模型调用，看着这段对话，猜用户接下来大概会写什么。
//!
//! 单独一个模块（不挂在 `Kernel` 上）：补全要跟着输入框的停顿随时发，不该进会话的
//! 工具循环，也不该记进对话日志。界面去抖之后调 [`complete`]（阻塞，在后台线程里跑），
//! 拿到一句话就画在光标后面，按 Tab 接上。
//!
//! 两点刻意的取舍：
//!
//! - **只给用户说过的话和最后的回复**。工具调用的输出又长又不该影响「用户接下来会说什么」，
//!   带着它只会把补全引向技术细节。助手最后一段回复要说的是什么，用户多半接着它写。
//! - **只输出那句补全本身**，不加引号、不解释。模型多写了就把这些清理掉（见 [`clean`]）；
//!   清理之后太长、或者和已经写了的重复，直接当没有（宁可没有，也不要一句驴唇不对马嘴的灰字）。

use crate::{ChatMessage, ProviderConfig, RequestParams, StreamEvent};

/// 补全最多多少个字（超了截到最后一个标点或词边界，见 [`clean`]）
pub const MAX_CHARS: usize = 60;

/// 系统提示词（固定模板）。
pub fn system_prompt() -> String {
    format!(
        "You complete the user's next message in a chat with a coding agent working on their own \
         computer.\n\
         \n\
         You see what the user has already typed (the draft) and the recent conversation. Write the \
         single short continuation that the user is most likely typing next: the rest of their \
         sentence, or the next sentence, as if the user were still typing it.\n\
         \n\
         Rules:\n\
         - Output only the continuation, in the same language as the draft and the conversation.\n\
         - No quotes, no explanation, no markdown syntax, no prefix such as \"Next:\".\n\
         - At most about {MAX_CHARS} characters; it is a continuation, not an answer.\n\
         - If the draft is empty, write the kind of thing the user would say next (a request, a \
           correction, a follow-up question).\n\
         - Never continue the assistant's voice, and do not answer your own continuation.\n\
         - If you cannot guess anything useful, output nothing at all."
    )
}

/// 拼给模型看的一次补全请求：草稿 + 最近的对话。
///
/// `history` 是按时间顺序的 (是不是用户说的, 文字)，调用方只放用户消息和助手的最后一段回复。
pub fn build_messages(draft: &str, history: &[(bool, String)]) -> Vec<ChatMessage> {
    let mut user = String::from("# Recent conversation\n");
    if history.is_empty() {
        user.push_str("(nothing yet, this is the first message)\n");
    }
    for (from_user, text) in history {
        let who = if *from_user { "user" } else { "assistant" };
        let text: String = text.trim().chars().take(1_500).collect();
        user.push_str(&format!("\n[{who}]\n{text}\n"));
    }
    user.push_str("\n# What the user has typed so far\n");
    if draft.trim().is_empty() {
        user.push_str("(empty)\n");
    } else {
        user.push_str(draft);
        user.push('\n');
    }
    user.push_str("\n# Continuation\n");
    vec![ChatMessage::new("system", system_prompt()), ChatMessage::new("user", user)]
}

/// 补全用的请求参数：补一句短话，温度稍高一点（不想要每次都是最平庸的那句），
/// 输出只要一行。原来配置里的 `extra` 照留（有的端点没有它就报错）。
fn params(base: &RequestParams) -> RequestParams {
    RequestParams {
        temperature: Some(0.5),
        max_tokens: Some(64),
        thinking_budget: None,
        server_tools: Vec::new(),
        extra: base.extra.clone(),
    }
}

/// 发一次补全请求（阻塞，调用方放在后台线程里）。返回清理过的一句；没有可补的就 None。
///
/// `session` / `turn` 只用来给用量网关的日志归类（用途 `suggest`），和会话本身的轮次无关。
pub fn complete(
    config: &ProviderConfig,
    session: &str,
    turn: u64,
    draft: &str,
    history: &[(bool, String)],
) -> Option<String> {
    // 温度 / 输出上限换成补全那一套（provider 在构造时就吃下参数）
    let config = ProviderConfig { params: params(&config.params), ..config.clone() };
    let provider = crate::make_provider(&config);
    let messages = build_messages(draft, history);
    let mut reply = String::new();
    let _tag = crate::gateway::tag(session, "suggest", turn);
    let outcome = provider.stream_chat(&config.model, &messages, &[], &|| false, &mut |event| {
        if let StreamEvent::Text(text) = event {
            reply.push_str(&text);
        }
    });
    outcome.ok()?;
    let text = clean(&reply);
    // 和已经写好的那部分重复（模型把草稿复述了一遍）：不如不显示
    (!text.is_empty() && !draft.trim_end().ends_with(text.as_str())).then_some(text)
}

/// 清理模型的回复：去掉引号 / 代码围栏 / 「下一句：」这类前缀，压成一行，按 [`MAX_CHARS`] 截断。
pub fn clean(reply: &str) -> String {
    // 去掉 ``` 围栏行（模型常把一句话包进代码块）
    let body = reply
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n");
    // 只取第一行有内容的部分（模型有时会多写一行解释）
    let text = body.trim();
    let text = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    // 引号 / 方括号 / 结尾的句号
    let text = text
        .trim_start_matches(['`', '"', '\'', '「', '『', '“', '‘', '[', ']'])
        .trim_end_matches(['`', '"', '\'', '」', '』', '”', '’']);
    // 「下一句：」「Next:」这类前缀
    let text = text
        .strip_prefix("下一句：")
        .or_else(|| text.strip_prefix("补全："))
        .or_else(|| text.strip_prefix("Next: "))
        .or_else(|| text.strip_prefix("Continuation: "))
        .unwrap_or(text)
        .trim();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= MAX_CHARS {
        return text;
    }
    // 太长：截到最后一个标点或空格，宁可短也不要半截词
    let clipped: String = text.chars().take(MAX_CHARS).collect();
    let cut = clipped
        .char_indices()
        .filter(|(_, c)| {
            matches!(c, '。' | '！' | '？' | '.' | '!' | '?' | '，' | ',' | '；' | ';')
        })
        .map(|(i, c)| i + c.len_utf8())
        .next_back()
        .or_else(|| clipped.rfind(' '))
        .filter(|i| *i > 0)
        .unwrap_or(clipped.len());
    clipped[..cut].trim_end_matches(['，', ',', '；', ';', '、']).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_last_draft_prompt() {
        let messages = build_messages("帮我看看", &[(true, "修一下测试".into())]);
        assert_eq!(messages.len(), 2);
        let user = messages[1].text().unwrap();
        assert!(user.contains("[user]\n修一下测试"), "{user}");
        assert!(user.contains("帮我看看"), "{user}");
        // 全新会话：说明一下没有历史
        let messages = build_messages("", &[]);
        assert!(messages[1].text().unwrap().contains("first message"));
        assert!(messages[1].text().unwrap().contains("(empty)"));
    }

    #[test]
    fn cleans_quotes_prefixes_and_lines() {
        assert_eq!(clean("「顺便把 README 也改一下」"), "顺便把 README 也改一下");
        assert_eq!(clean("```\n下一句：再跑一遍测试\n```"), "再跑一遍测试");
        assert_eq!(clean("Next: run the tests again"), "run the tests again");
        assert_eq!(clean("  \n\n "), "");
        assert_eq!(clean("line one\nline two"), "line one");
    }

    #[test]
    fn clips_to_max_chars() {
        let long = "这是一句很长的话，".repeat(20);
        let text = clean(&long);
        assert!(text.chars().count() <= MAX_CHARS, "{text}");
        // 结尾的逗号去掉（补全不该停在一个逗号上）
        assert!(!text.ends_with('，'), "{text}");
        // 带句号的：截在句号上
        let sentences = "这是一句很长的话。".repeat(20);
        let text = clean(&sentences);
        assert!(text.chars().count() <= MAX_CHARS, "{text}");
        assert!(text.ends_with('。'), "{text}");
        // 没有标点的长英文按词边界截
        let words = "word ".repeat(40);
        let text = clean(&words);
        assert!(text.chars().count() <= MAX_CHARS, "{text}");
        assert!(!text.ends_with("wor"), "{text}");
    }
}
