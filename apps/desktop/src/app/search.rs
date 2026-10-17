//! 搜消息：命令面板（Ctrl+K）里打字时，在已载入的会话里搜用户消息和回复的正文，
//! 命中的一条列成「消息」组（摘一小段上下文），确认后切到那个会话、滚到那条消息并闪一下底色。
//!
//! 只搜内存里的会话（侧栏里那些，见 `MAX_RESTORED`）；会话标题由面板的「会话」组自己按名字过滤。

use super::ChatApp;
use crate::model::ChatItem;
use crate::session::Session;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::*;
use std::time::{Duration, Instant};

/// 至少打几个字才搜正文（一个字命中太多，列出来也没法挑）
const MIN_QUERY_CHARS: usize = 2;
/// 一个会话最多列几条、总共最多列几条
const PER_SESSION: usize = 3;
const MAX_HITS: usize = 30;
/// 摘要里命中处前面留几个字、一共最多多少字
const SNIPPET_BEFORE: usize = 12;
const SNIPPET_LEN: usize = 60;
/// 跳过去后那条消息的底色先停一会儿，再淡掉
const FLASH_HOLD: Duration = Duration::from_millis(1200);
const FLASH_FADE: Duration = Duration::from_millis(600);

/// 一条命中的消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageHit {
    /// 会话下标（`ChatApp::sessions`）
    pub session: usize,
    /// 消息在对话里的行号
    pub item: usize,
    pub from_user: bool,
    /// 命中处附近的一小段（压成一行）
    pub snippet: String,
}

/// 在 `order`（会话下标，按侧栏顺序）列出的会话里搜 `query`（不分大小写）。
pub(crate) fn search_messages(
    sessions: &[Session],
    order: &[usize],
    query: &str,
) -> Vec<MessageHit> {
    let needle = query.trim().to_lowercase();
    if needle.chars().count() < MIN_QUERY_CHARS {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for &session in order {
        let mut n = 0;
        for (item, row) in sessions[session].transcript.iter().enumerate() {
            let (text, from_user) = match row {
                ChatItem::User { text, .. } => (text.as_ref(), true),
                ChatItem::Agent(text) => (text.as_str(), false),
                _ => continue,
            };
            if let Some(snippet) = snippet(text, &needle) {
                hits.push(MessageHit { session, item, from_user, snippet });
                n += 1;
                if hits.len() >= MAX_HITS {
                    return hits;
                }
                if n >= PER_SESSION {
                    break;
                }
            }
        }
    }
    hits
}

/// `text` 里有 `needle`（已转小写）就摘出命中处附近的一段：前面留几个字，空白压成一个空格，
/// 截掉的头尾写省略号。没有命中为 None。
fn snippet(text: &str, needle: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let at = lower.find(needle)?;
    // 少数字符转小写后字节数会变，偏移对不上就从头摘
    let at = if lower.len() == text.len() && text.is_char_boundary(at) { at } else { 0 };
    let before: Vec<char> = text[..at].chars().collect();
    let start = before.len().saturating_sub(SNIPPET_BEFORE);
    let head: String = before[start..].iter().collect();
    let piece: String = head.chars().chain(text[at..].chars()).take(SNIPPET_LEN).collect();
    let mut out = piece.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        out.insert(0, '…');
    }
    if text.chars().count() - start > SNIPPET_LEN {
        out.push('…');
    }
    Some(out)
}

impl ChatApp {
    /// 面板里的查询变了：重新搜正文（面板每次重画都读这份结果，不在重画时搜）。
    pub(super) fn search_palette(&mut self, query: &str) {
        let order: Vec<usize> =
            self.session_groups().into_iter().flat_map(|(_, members)| members).collect();
        self.palette_hits = search_messages(&self.sessions, &order, query);
    }

    /// 从搜索结果跳到一条消息：切会话、滚过去（`Session::reveal`，列表同步时生效）、闪一下底色。
    pub(super) fn reveal_message(&mut self, session: usize, item: usize, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.activate_session(session, cx);
        let s = &mut self.sessions[session];
        s.reveal = Some(item);
        self.flash = Some((s.token, item, Instant::now()));
    }

    /// 第 `ix` 行刚从搜索结果跳过来：返回底色（先停一会儿再淡掉），走完为 None。
    pub(super) fn flash_bg(&self, ix: usize, window: &mut Window, cx: &App) -> Option<Rgba> {
        let (token, item, at) = self.flash?;
        if token != self.session().token || item != ix {
            return None;
        }
        let age = at.elapsed();
        if age >= FLASH_HOLD + FLASH_FADE {
            return None;
        }
        window.request_animation_frame();
        let alpha = if age < FLASH_HOLD || cx.reduce_motion() {
            1.
        } else {
            1. - (age - FLASH_HOLD).as_secs_f32() / FLASH_FADE.as_secs_f32()
        };
        let tint = rgb(SELECTED);
        Some(Rgba { a: tint.a * alpha, ..tint })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::KernelChoice;
    use std::path::PathBuf;

    fn session(rows: Vec<ChatItem>) -> Session {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.transcript = rows;
        s
    }

    fn user(text: &str) -> ChatItem {
        ChatItem::User { text: text.into(), images: Vec::new() }
    }

    #[core::prelude::v1::test]
    fn finds_user_and_agent_text_case_insensitively() {
        let a = session(vec![
            user("帮我看看 Kernel trait"),
            ChatItem::Agent("kernel 拆成两个更好".into()),
            ChatItem::Note("kernel 备注不搜".into()),
        ]);
        let b = session(vec![user("别的事")]);
        let hits = search_messages(&[a, b], &[1, 0], "KERNEL");
        assert_eq!(
            hits.iter().map(|h| (h.session, h.item, h.from_user)).collect::<Vec<_>>(),
            [(0, 0, true), (0, 1, false)]
        );
        // 太短不搜
        assert!(search_messages(&[session(vec![user("ab")])], &[0], "a").is_empty());
    }

    #[core::prelude::v1::test]
    fn caps_hits_per_session() {
        let rows = (0..10).map(|i| user(&format!("第 {i} 条 needle"))).collect();
        let hits = search_messages(&[session(rows)], &[0], "needle");
        assert_eq!(hits.len(), PER_SESSION);
    }

    #[core::prelude::v1::test]
    fn snippet_keeps_context_on_one_line() {
        let text = format!(
            "{}\n\n命中的词在这里，后面还有{}",
            "很长的开头".repeat(10),
            "很多字".repeat(30)
        );
        let s = snippet(&text, "命中的词").unwrap();
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("命中的词") && !s.contains('\n'), "{s}");
        assert_eq!(snippet("短句 hit", "hit").unwrap(), "短句 hit");
        assert_eq!(snippet("没有", "hit"), None);
    }
}
