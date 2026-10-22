//! 一轮结束后的「候选记忆」：看这段对话，提炼出值得留到以后的一句话结论，
//! **只交给用户确认**（设置页「记忆」顶部的待确认区），永远不直接写文件。
//!
//! 为什么单独一个模块、为什么只产出候选：
//!
//! - 误记的代价 > 漏记的代价：漏了用户下一句提醒就行，误记要人去设置页翻、读、删，
//!   几次之后就不看那页了（见「设计说明-记忆系统.md」第四节）。所以这里的结果先挂起。
//! - 一次独立的小请求（`name_session` 是先例），和其他请求一样经用量网关记一笔（用途 `memory`）。
//! - 每轮正常结束后跑一次（子会话不跑）、且默认关：一次请求换一句可能用得上的话，只在用户明确要的时候花。

use crate::{ChatMessage, ProviderConfig, RequestParams, StreamEvent};
use claudesktop_core::MemoryCandidateView;
use claudesktop_core::memory::{self, Entry};

/// 一次最多提炼几条
pub const MAX_CANDIDATES: usize = 3;
/// 候选的一句话最多多少字（和落盘的条目同一个上限）
pub const MAX_CHARS: usize = memory::MAX_TEXT_CHARS;
/// 灌进提炼请求的对话最多多少字符（再长就截）
const TRANSCRIPT_CHARS: usize = 6_000;

/// 提炼用的系统提示词（固定模板）。
pub fn system_prompt() -> String {
    "You review one exchange between a user and a coding agent working on the user's own \
     computer, and pull out anything worth remembering in later sessions.\n\
     \n\
     Worth remembering: a user preference, a project convention, a decision with its reason, or a \
     pitfall that was hit. Not worth it: what the repository already says (layout, dependency \
     versions, function signatures), details of this one task, a play-by-play of the tool calls, \
     or anything the user did not actually say or confirm. Never include tokens, passwords, keys \
     or other credentials.\n\
     \n\
     Output at most three lines, each one note: a single sentence with the why when it matters, \
     in the user's language, no bullets, no quotes, no commentary. If there is nothing worth \
     remembering, output nothing at all — that is the usual answer."
        .to_string()
}

/// 提炼的请求参数：一句话一条，输出很短。
///
/// 档位和补全一样按模型覆盖（能关就关、关不掉取最低一档）：提炼只要长一点的判断，
/// 不需要「跟着会话的档位」那样深度思考。
fn params(base: &RequestParams, protocol: crate::Protocol, model: &str) -> RequestParams {
    let mut extra = base.extra.clone();
    if let Some(map) = extra.as_object_mut() {
        for key in
            ["thinking", "reasoning", "reasoning_effort", "enable_thinking", "chat_template_kwargs"]
        {
            map.remove(key);
        }
    }
    RequestParams {
        temperature: Some(0.2),
        max_tokens: Some(400),
        thinking: crate::suggest::short_thinking(protocol, model),
        thinking_budget: None,
        server_tools: Vec::new(),
        extra,
    }
}

/// 拼给模型看的东西：已有的记忆（免得再提一遍）+ 这一轮的对话。
pub fn build_messages(known: &[&str], user: &str, assistant: &str) -> Vec<ChatMessage> {
    let mut body = String::new();
    if !known.is_empty() {
        body.push_str("# Notes already saved (do not repeat them)\n");
        for note in known {
            body.push_str(&format!("- {note}\n"));
        }
        body.push('\n');
    }
    body.push_str("# The exchange\n\n[user]\n");
    body.push_str(&user.chars().take(TRANSCRIPT_CHARS / 2).collect::<String>());
    body.push_str("\n\n[assistant]\n");
    body.push_str(&assistant.chars().take(TRANSCRIPT_CHARS / 2).collect::<String>());
    body.push_str("\n\n# Notes worth remembering\n");
    vec![ChatMessage::new("system", system_prompt()), ChatMessage::new("user", body)]
}

/// 发一次提炼请求（阻塞，调用方放在后台线程里）。失败 / 没内容返回空表。
///
/// `model` 用会话当前的模型；`session` / `turn` 只用来给用量网关的日志归类（用途 `memory`）。
#[allow(clippy::too_many_arguments)]
pub fn suggest(
    config: &ProviderConfig,
    model: &str,
    session: &str,
    turn: u64,
    known: &[&str],
    user: &str,
    assistant: &str,
    should_stop: &dyn Fn() -> bool,
) -> Vec<MemoryCandidateView> {
    if user.trim().is_empty() || assistant.trim().is_empty() {
        return Vec::new();
    }
    let config =
        ProviderConfig { params: params(&config.params, config.protocol, model), ..config.clone() };
    let provider = crate::make_provider(&config);
    let mut reply = String::new();
    let _tag = crate::gateway::tag(session, "memory", turn);
    let messages = build_messages(known, user, assistant);
    let outcome = provider.stream_chat(model, &messages, &[], should_stop, &mut |event| {
        if let StreamEvent::Text(text) = event {
            reply.push_str(&text);
        }
    });
    if outcome.is_err() || should_stop() {
        return Vec::new();
    }
    parse(&reply)
}

/// 把模型的回复拆成候选：一行一条，去掉列表符号和引号，截到 [`MAX_CHARS`]，
/// 和已有的、以及自己前面那条重复的丢掉。
pub fn parse(reply: &str) -> Vec<MemoryCandidateView> {
    let mut out: Vec<MemoryCandidateView> = Vec::new();
    for line in reply.lines() {
        let text = clean_line(line);
        if text.is_empty() {
            continue;
        }
        // 同一句换个标点再说一遍：算重复
        let entry = Entry::new(text.as_str(), 0);
        if out.iter().any(|c| same(&c.text, &entry.text)) {
            continue;
        }
        out.push(MemoryCandidateView { key: key(&entry.text), text: entry.text, tags: Vec::new() });
        if out.len() >= MAX_CANDIDATES {
            break;
        }
    }
    out
}

/// 一行 → 一句：去掉「- 」「1. 」「* 」这类列表符号、引号和首尾空白；
/// 「没有」这类明确的否定回答不算。太长按 [`MAX_CHARS`] 截（`Entry::new` 干这件事）。
fn clean_line(line: &str) -> String {
    let text = line.trim();
    // 列表符号 / 序号
    let text = text
        .trim_start_matches(['-', '*', '•'])
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(['.', ')', '、'])
        .trim();
    let text = text.trim_matches(['"', '\'', '「', '」', '『', '』', '“', '”', '`']).trim();
    if text.is_empty() || text.chars().count() < 4 || is_nothing(text) {
        return String::new();
    }
    text.to_string()
}

/// 模型说「没什么可记的」时的几种写法（别把它当成一条记忆）。
fn is_nothing(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let head = lower.trim_end_matches('.').trim();
    if matches!(head, "nothing" | "none" | "n/a" | "no notes" | "nothing worth remembering") {
        return true;
    }
    // 英文的整句否定（「Nothing worth remembering.」）
    if head.starts_with("nothing") && head.chars().count() <= 60 {
        return true;
    }
    // 中文：只有「没有」这一句（更长的多半是真实的一条，比如「没有用 pnpm 是因为…」）
    text.chars().count() <= 12
        && ["没有", "无", "没有值得", "无值得"].iter().any(|n| text.starts_with(n))
}

/// 两条是不是同一句（忽略大小写、空白、标点）。
fn same(a: &str, b: &str) -> bool {
    key(a) == key(b)
}

/// 比相似用的键：只留字母、数字、CJK，转小写（和 `memory::upsert` 的判据一致）。
pub fn key(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_into_notes() {
        let reply = "- 构建只用 cargo test -p claudesktop-core，全量太慢\n\
                     2. 「回复一律用中文」\n\
                     * 回复一律用中文。\n\
                     - 没有\n";
        let out = parse(reply);
        // 第三条和第二条是同一句，丢掉
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0].text, "构建只用 cargo test -p claudesktop-core，全量太慢");
        assert_eq!(out[1].text, "回复一律用中文");
        // 键按「只留字母数字」算，同一句换个标点也一样
        assert_eq!(key("回复一律用中文。"), key("回复一律用中文"));
    }

    #[test]
    fn caps_the_number_and_length() {
        let long = "很长的一句话".repeat(200);
        let reply = format!("- {long}\n- 第二句话在这里\n- 第三句话在这里\n- 第四句话在这里\n");
        let out = parse(&reply);
        assert_eq!(out.len(), MAX_CANDIDATES);
        assert!(out[0].text.chars().count() <= MAX_CHARS + 1);
        assert!(out[0].text.ends_with('…'));
    }

    #[test]
    fn empty_and_negative_answers_yield_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("Nothing worth remembering.").is_empty());
        assert!(parse("没有").is_empty());
        assert!(parse("- \n- \n").is_empty());
        // 真的记了一条「没有…」的话，不能被当成否定回答丢掉
        assert_eq!(parse("没有用 pnpm 是因为锁文件冲突").len(), 1);
    }

    #[test]
    fn build_messages_lists_known_notes_and_the_exchange() {
        let messages = build_messages(&["回复一律用中文"], "以后构建只用 core", "好");
        assert_eq!(messages.len(), 2);
        let body = messages[1].text().unwrap();
        assert!(body.contains("Notes already saved"), "{body}");
        assert!(body.contains("回复一律用中文"), "{body}");
        assert!(body.contains("[user]\n以后构建只用 core"), "{body}");
        assert!(body.contains("[assistant]\n好"), "{body}");
        let system = messages[0].text().unwrap();
        assert!(system.contains("tokens, passwords"), "{system}");
    }

    #[test]
    fn drops_thinking_switches_from_extra() {
        let base = RequestParams {
            extra: serde_json::json!({"enable_thinking": true, "top_p": 0.9}),
            ..Default::default()
        };
        let p = params(&base, crate::Protocol::OpenAiCompat, "some-model");
        assert_eq!(p.extra, serde_json::json!({"top_p": 0.9}));
        assert_eq!(p.max_tokens, Some(400));
    }
}
