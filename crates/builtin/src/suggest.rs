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
         - If the continuation starts a new word after the draft, begin it with a space.\n\
         - At most about {MAX_CHARS} characters; it is a continuation, not an answer.\n\
         - If the draft is empty, write the kind of thing the user would say next (a request, a \
           correction, a follow-up question).\n\
         - Never continue the assistant's voice, and do not answer your own continuation.\n\
         - If you cannot guess anything useful, output nothing at all."
    )
}

/// 「一轮结束后猜下一句」的系统提示词（固定模板）。
///
/// 和 [`system_prompt`] 的区别：那边是「接着用户没写完的半句往下写」，这边输入框是空的，
/// 猜的是用户看完这一轮回复之后最可能说的一句话——所以要求它给一整句能直接发出去的请求。
pub fn next_message_prompt() -> String {
    format!(
        "You predict the user's next message in a chat with a coding agent working on their own \
         computer. The agent has just finished a turn; the user has not typed anything yet.\n\
         \n\
         You see the recent conversation. Write the single message the user is most likely to send \
         next, as if the user were typing it themselves.\n\
         \n\
         Rules:\n\
         - Output only that one message, in the same language as the conversation.\n\
         - No quotes, no explanation, no markdown syntax, no prefix such as \"Next:\".\n\
         - A complete, usable message the user could send as-is; not an answer to the user's \
           request, and never in the agent's voice.\n\
         - When the agent asked a question or offered options, the message is the user's reply to \
           it (a choice, a yes/no, or a follow-up detail).\n\
         - When the agent reported a result, the message is the obvious next step (run it, fix \
           what broke, commit, explain a piece of it).\n\
         - At most about {MAX_CHARS} characters.\n\
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
/// 输出只要一行。原来配置里的 `extra` 照留（有的端点没有它就报错），但去掉开思考的键：
/// 64 个 token 不够思考用，开着就补不出字。
fn params(base: &RequestParams) -> RequestParams {
    let mut extra = base.extra.clone();
    if let Some(map) = extra.as_object_mut() {
        for key in REASONING_KEYS {
            map.remove(*key);
        }
    }
    RequestParams {
        temperature: Some(0.5),
        max_tokens: Some(64),
        thinking_budget: None,
        server_tools: Vec::new(),
        extra,
    }
}

/// `extra` 里开思考 / 推理的键（各家写法）
const REASONING_KEYS: &[&str] =
    &["thinking", "reasoning", "reasoning_effort", "enable_thinking", "chat_template_kwargs"];

/// 看名字是不是总要先思考的模型（关不掉思考，补全的 64 个 token 会被思考用光）。
/// 只认常见的几种命名；认不出的照常发，补不出来也只是没有灰字。
pub fn is_reasoning_model(model: &str) -> bool {
    let name = model.rsplit('/').next().unwrap_or(model).to_ascii_lowercase();
    let o_series =
        ["o1", "o3", "o4"].iter().any(|p| name == *p || name.starts_with(&format!("{p}-")));
    o_series
        || name.contains("reasoner")
        || name.contains("thinking")
        || name.split(['-', '_', ':']).any(|part| part == "r1" || part == "qwq")
}

/// 发一次补全请求（阻塞，调用方放在后台线程里）。返回清理过的一句；没有可补的就 None。
///
/// `session` / `turn` 只用来给用量网关的日志归类（用途 `suggest`），和会话本身的轮次无关。
///
/// `should_stop` 返回 true 时中断（用户又打字了，这次的结果用不上）。
pub fn complete(
    config: &ProviderConfig,
    session: &str,
    turn: u64,
    draft: &str,
    history: &[(bool, String)],
    should_stop: &dyn Fn() -> bool,
) -> Option<String> {
    let reply = ask(config, session, turn, build_messages(draft, history), should_stop)?;
    let text = clean(&reply);
    // 和已经写好的那部分重复（模型把草稿复述了一遍）：不如不显示
    if text.is_empty() || draft.trim_end().ends_with(text.as_str()) {
        return None;
    }
    Some(join(draft, &reply, text))
}

/// 一轮结束后猜用户下一句会说什么（阻塞，调用方放在后台线程里）。空草稿调 [`complete`]，
/// 只是系统提示词换成了 [`next_message_prompt`]：这里不接用户已经写的东西，给的是整句。
pub fn complete_next(
    config: &ProviderConfig,
    session: &str,
    turn: u64,
    history: &[(bool, String)],
    should_stop: &dyn Fn() -> bool,
) -> Option<String> {
    let mut messages = build_messages("", history);
    if let Some(system) = messages.first_mut() {
        *system = ChatMessage::new("system", next_message_prompt());
    }
    let reply = ask(config, session, turn, messages, should_stop)?;
    let text = clean(&reply);
    (!text.is_empty()).then_some(text)
}

/// 发一次流式请求，把正文拼起来（补全和「猜下一句」共用）。
fn ask(
    config: &ProviderConfig,
    session: &str,
    turn: u64,
    messages: Vec<ChatMessage>,
    should_stop: &dyn Fn() -> bool,
) -> Option<String> {
    // 温度 / 输出上限换成补全那一套（provider 在构造时就吃下参数）
    let config = ProviderConfig { params: params(&config.params), ..config.clone() };
    let provider = crate::make_provider(&config);
    let mut reply = String::new();
    let _tag = crate::gateway::tag(session, "suggest", turn);
    let outcome = provider.stream_chat(&config.model, &messages, &[], should_stop, &mut |event| {
        if let StreamEvent::Text(text) = event {
            reply.push_str(&text);
        }
    });
    outcome.ok()?;
    if should_stop() {
        return None;
    }
    Some(reply)
}

/// 接在草稿后面要不要先补一个空格：[`clean`] 把回复首尾的空白都去掉了，
/// 但英文里「新起一个词」靠的就是开头那个空格（`fix the` + ` tests`）。
/// 模型回复开头带空白，或者草稿停在标点上、补全以字母数字开头，就补一个；
/// 草稿已经以空白结尾、或者两边有一边是中日韩文字时不补。
fn join(draft: &str, reply: &str, text: String) -> String {
    let (Some(last), Some(first)) = (draft.chars().next_back(), text.chars().next()) else {
        return text;
    };
    if last.is_whitespace() || !last.is_ascii() || !first.is_ascii() {
        return text;
    }
    let spaced = reply.trim_start_matches(['\n', '\r']).starts_with([' ', '\t']);
    let after_punct =
        matches!(last, ',' | '.' | ';' | ':' | '!' | '?') && first.is_ascii_alphanumeric();
    if spaced || after_punct { format!(" {text}") } else { text }
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
        .trim_start_matches(['`', '"', '\'', '「', '『', '“', '‘', '['])
        .trim_end_matches(['`', '"', '\'', '」', '』', '”', '’', ']']);
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

    /// 轮后猜下一句：系统提示词换成「写用户接下来会发的那句话」，不接用户已经写的东西。
    #[test]
    fn next_message_prompt_asks_for_a_whole_message() {
        let system = next_message_prompt();
        assert!(system.contains("most likely to send"), "{system}");
        assert!(system.contains("same language"), "{system}");
        assert!(!system.contains("draft"), "{system}");
        // 用户那侧仍是「还没写」的空草稿
        assert!(build_messages("", &[])[1].text().unwrap().contains("(empty)"));
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
    fn keeps_the_space_before_a_new_word() {
        let joined = |draft: &str, reply: &str| join(draft, reply, clean(reply));
        // 模型回复开头的空格是「新起一个词」，不能被吃掉
        assert_eq!(joined("fix the", " tests"), " tests");
        // 接着半个词往下写
        assert_eq!(joined("fix the te", "sts"), "sts");
        // 草稿已经以空格结尾
        assert_eq!(joined("fix the ", " tests"), "tests");
        // 停在标点上
        assert_eq!(joined("done.", "Now run it"), " Now run it");
        // 中文不加空格
        assert_eq!(joined("帮我看看", " 测试为什么挂了"), "测试为什么挂了");
    }

    #[test]
    fn spots_models_that_always_think() {
        for name in [
            "deepseek-reasoner",
            "deepseek-r1",
            "deepseek/deepseek-r1:free",
            "o3-mini",
            "o1",
            "qwq-32b",
            "claude-3-7-sonnet-thinking",
        ] {
            assert!(is_reasoning_model(name), "{name}");
        }
        for name in
            ["deepseek-chat", "glm-4-flash", "gpt-4o-mini", "claude-haiku-4-5", "qwen3-coder"]
        {
            assert!(!is_reasoning_model(name), "{name}");
        }
    }

    #[test]
    fn drops_thinking_switches_from_extra() {
        let base = RequestParams {
            extra: serde_json::json!({"reasoning_effort": "high", "top_p": 0.9}),
            ..Default::default()
        };
        let p = params(&base);
        assert_eq!(p.extra, serde_json::json!({"top_p": 0.9}));
        assert_eq!(p.max_tokens, Some(64));
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
