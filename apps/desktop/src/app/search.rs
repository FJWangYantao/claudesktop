//! 搜消息：命令面板（Ctrl+K）里打字时，在已载入的会话里搜用户消息和回复的正文，
//! 命中的一条列成「消息」组（摘一小段上下文），确认后切到那个会话、滚到那条消息并闪一下底色。
//!
//! 内存里只有最近的一百个会话（`MAX_RESTORED`），更早的搜不到；所以同时让
//! [`claudesktop_core::history_search`] 在后台线程里扫磁盘上的会话日志，
//! 命中的列在「历史」组（会话还在磁盘上、没载入内存的那些），选中后把它恢复进侧栏再跳过去。

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
/// 查询变了之后隔多久才真去搜（见 `ChatApp::search_palette`）
const SEARCH_DEBOUNCE_MS: u64 = 150;
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

/// 磁盘上搜到的一条命中（会话还没载入内存的那些）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryHitState {
    /// 会话 ID（日志文件名）
    pub session: String,
    /// 第几条用户消息（从 0 数）
    pub turn: usize,
    pub from_user: bool,
    pub snippet: String,
    /// 会话标题（从元信息读；读不到时用那一轮的用户消息开头）
    pub title: String,
}

/// 后台线程里跑：扫磁盘上的会话日志，跳过已经载入内存的会话。
pub(crate) fn scan_history(
    dir: &std::path::Path,
    loaded: &std::collections::HashSet<String>,
    query: &str,
) -> Vec<HistoryHitState> {
    let files = claudesktop_core::history_search::log_files(dir)
        .into_iter()
        .filter(|(id, _)| !loaded.contains(id));
    let hits = claudesktop_core::history_search::search_logs(files, query);
    // 标题从元信息读（每个会话的 json 文件），读不到的用那一轮的用户消息开头
    let titles = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let meta: claudesktop_core::SessionMeta =
                serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
            Some((meta.id, meta.title))
        })
        .collect::<std::collections::HashMap<_, _>>();
    hits.into_iter()
        .map(|hit| {
            let title = titles.get(&hit.session).cloned().unwrap_or(hit.prompt);
            HistoryHitState {
                session: hit.session,
                turn: hit.turn,
                from_user: hit.from_user,
                snippet: hit.snippet,
                title,
            }
        })
        .collect()
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
    // 命中处后面还有内容才补尾巴的省略号（`start` 是字符下标，不会超过字符总数）
    if text.chars().count().saturating_sub(start) > SNIPPET_LEN {
        out.push('…');
    }
    Some(out)
}

impl ChatApp {
    /// 面板里的查询变了：重新搜正文（面板每次重画都读这份结果，不在重画时搜）。
    ///
    /// 排到下一次节拍再搜：`on_query` 每敲一个字都会来，而在 UI 线程上把上百个会话的
    /// 正文整段小写化一遍是实打实的开销。查询变了才搜（连按两次同一个字只搜一次），
    /// 面板那时已经画过一次，列表跟着更新。磁盘上的更早会话在后台线程里扫（见 [`scan_history`]）。
    pub(super) fn search_palette(&mut self, query: &str, cx: &mut Context<Self>) {
        let query = query.trim().to_lowercase();
        if query == self.palette_query {
            return;
        }
        self.palette_query = query;
        let generation = self.palette_generation;
        // 150ms 防抖：还没到点又敲下一个字时，旧的那个 `timer` 掉了就自行取消
        self.palette_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(SEARCH_DEBOUNCE_MS))
                .await;
            this.update(cx, |this, cx| {
                // 面板关了或者又敲了字：这次的结果已经过期
                if this.palette_generation != generation {
                    return;
                }
                let query = this.palette_query.clone();
                let order: Vec<usize> =
                    this.session_groups().into_iter().flat_map(|g| g.members).collect();
                this.palette_hits = search_messages(&this.sessions, &order, &query);
                // 已经在内存里的会话（侧栏有）不再重复扫磁盘
                let loaded: std::collections::HashSet<String> = this
                    .sessions
                    .iter()
                    .filter_map(|s| s.meta.as_ref().map(|m| m.id.clone()))
                    .collect();
                let dir = this.store.as_ref().map(|s| s.dir().to_path_buf());
                this.palette_history.clear();
                cx.notify();
                let Some(dir) = dir else { return };
                let query = query.clone();
                let want = query.clone();
                // 扫磁盘放在后台线程：几百个会话逐个读文件不能占 UI 线程
                cx.spawn(async move |this, cx| {
                    let hits = cx
                        .background_executor()
                        .spawn(async move { scan_history(&dir, &loaded, &query) })
                        .await;
                    this.update(cx, |this, cx| {
                        if this.palette_query != want || this.palette_generation != generation {
                            return;
                        }
                        this.palette_history = hits;
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            })
            .ok();
        }));
    }

    /// 面板关掉 / 重新打开：清掉上一次的查询和待搜的节拍。
    pub(super) fn reset_palette_search(&mut self) {
        self.palette_query.clear();
        self.palette_generation += 1;
        self.palette_timer = None;
        self.palette_hits.clear();
        self.palette_history.clear();
    }

    /// 从历史搜索结果跳过去：那个会话可能还在磁盘上（没载入内存），先恢复进侧栏再定位。
    pub(super) fn reveal_history_hit(&mut self, hit: &HistoryHitState, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else { return };
        let turn = hit.turn;
        // 已经在内存里（同一个会话 ID）：直接跳
        let existing =
            self.sessions.iter().position(|s| s.meta.as_ref().is_some_and(|m| m.id == hit.session));
        let idx = match existing {
            Some(i) => i,
            None => {
                let Some(meta) = store.list().into_iter().find(|m| m.id == hit.session) else {
                    return;
                };
                let Ok(records) = store.load(&meta.id) else { return };
                let session =
                    Session::from_records(self.next_token, meta, records, &self.settings.acp);
                self.next_token += 1;
                self.sessions.push(session);
                self.sessions.len() - 1
            }
        };
        self.settings_open = false;
        self.activate_session(idx, cx);
        // 定位到那一轮的用户消息
        let item = self.sessions[idx]
            .transcript
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, ChatItem::User { .. }))
            .nth(turn)
            .map(|(i, _)| i);
        if let Some(item) = item {
            self.sessions[idx].reveal = Some(item);
            self.flash = Some((self.sessions[idx].token, item, Instant::now()));
        }
    }

    /// 从搜索结果跳到一条消息：切会话、滚过去（`Session::reveal`，列表同步时生效）、闪一下底色。
    /// 结果缓存在 `palette_hits` 上、是上次查询的快照，下标可能已经过期（会话被删 / 回退过），
    /// 一律先检查再动手。
    pub(super) fn reveal_message(&mut self, session: usize, item: usize, cx: &mut Context<Self>) {
        let Some(s) = self.sessions.get(session) else { return };
        if item >= s.transcript.len() {
            return;
        }
        self.settings_open = false;
        self.activate_session(session, cx);
        let s = &mut self.sessions[session];
        s.reveal = Some(item);
        self.flash = Some((s.token, item, Instant::now()));
    }

    /// 第 `ix` 行刚跳过来：给这一行套上高亮（先停一会儿再淡掉）。只换颜色、不动行高
    /// （虚拟列表不会重测）。消息铺一层底色；diff 卡片自己有底色，盖不住，改成沿卡片描一圈
    /// （`card` = 卡片下面留的行距，描边不框进去）。
    pub(super) fn with_flash(
        &self,
        row: Div,
        ix: usize,
        card: Option<Pixels>,
        window: &mut Window,
        cx: &App,
    ) -> Div {
        let Some(alpha) = self.flash_alpha(ix, window, cx) else { return row };
        if let Some(gap) = card {
            let ring = rgb(CORAL);
            row.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom(gap)
                    .rounded_lg()
                    .border_2()
                    .border_color(Rgba { a: ring.a * alpha, ..ring }),
            )
        } else {
            let tint = rgb(SELECTED);
            row.bg(Rgba { a: tint.a * alpha, ..tint }).rounded_xl()
        }
    }

    /// 第 `ix` 行的高亮还剩多少（1 = 正停着，往 0 淡），走完为 None。
    fn flash_alpha(&self, ix: usize, window: &mut Window, cx: &App) -> Option<f32> {
        let (token, item, at) = self.flash?;
        if token != self.session().token || item != ix {
            return None;
        }
        let age = at.elapsed();
        if age >= FLASH_HOLD + FLASH_FADE {
            return None;
        }
        window.request_animation_frame();
        Some(if age < FLASH_HOLD || cx.reduce_motion() {
            1.
        } else {
            1. - (age - FLASH_HOLD).as_secs_f32() / FLASH_FADE.as_secs_f32()
        })
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
        ChatItem::User { text: text.into(), images: Vec::new(), files: Default::default() }
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
