//! 逐块审查改动：最新一轮回复末尾改动卡片上的「审查」，按文件、按块撤回 / 恢复 agent 这一轮的改动；
//! 卡片上的「撤回」走同一套，整轮一次撤回（[`ChatApp::undo_turn`]）。
//!
//! - 打开时读一次日志和 blob（[`ReviewPage`]），每次写盘后只重算那一个文件；页面占内容区，Esc 或「返回」回到对话。
//! - 块、拼接、每个文件的 base / head / 现状在 `claudesktop_core::review`；写盘后追加一条带 `reverted`
//!   的 `Record::FileChanged`，回退照常算得对。
//! - 磁盘上的现状和日志里的对不上（agent 改完后又被改过）时整个文件只读，不做合并。
//! - 撤回过的块在下一条消息发出时写成一段说明附在前面（`take_review_note`），模型才知道。
//!
//! 设计见仓库根目录「设计说明-逐块审查改动.md」。

use super::ChatApp;
use super::chat::diff_line;
use super::menu::diff_stat;
use super::rewind::file_name;
use super::widgets::Surface;
use crate::model::{ChatItem, same_path};
use crate::session::TurnUndo;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::Record;
use claudesktop_core::checkpoint::{self, content_hash};
use claudesktop_core::review::{self, FileReview, Hunk, HunkRange};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::Path;

/// 一块最多画几行（其余写「还有 N 行」）
const HUNK_LINES_MAX: usize = 200;

/// 审查页：打开时算好，写盘后更新对应的文件。
pub(crate) struct ReviewPage {
    /// 哪个会话（令牌）
    token: u64,
    /// 第几条用户消息那一轮（从 0 数）：发出新消息后对不上，页面自动关掉
    turn: usize,
    files: Vec<ReviewFile>,
    /// 这一轮跑过几条命令（它们改的文件不在这里）
    commands: usize,
}

struct ReviewFile {
    /// 显示用路径（相对工作目录）
    display: String,
    state: FileState,
}

enum FileState {
    /// 能逐块撤回
    Ready { review: FileReview, base: Option<String>, head: Option<String>, hunks: Vec<Hunk> },
    /// agent 改完后磁盘上又被改过：只读
    Conflict { added: u32, removed: u32 },
    /// 没有检查点快照（太大、不是文本、ACP agent 自己直接写盘）：只读
    NoSnapshot { added: u32, removed: u32 },
}

impl ReviewFile {
    fn stat(&self) -> (u32, u32) {
        match &self.state {
            FileState::Ready { hunks, .. } => {
                hunks.iter().fold((0, 0), |(a, r), h| (a + h.added(), r + h.removed()))
            }
            FileState::Conflict { added, removed } | FileState::NoSnapshot { added, removed } => {
                (*added, *removed)
            }
        }
    }

    /// 撤回了几块
    fn reverted(&self) -> usize {
        match &self.state {
            FileState::Ready { review, hunks, .. } => {
                hunks.iter().filter(|h| review.reverted.contains(&h.range)).count()
            }
            _ => 0,
        }
    }
}

/// 读一份 blob；None 哈希 = 文件那时不存在。读不到时为 Err。
fn load(
    store: &claudesktop_core::SessionStore,
    id: &str,
    hash: &Option<String>,
) -> Result<Option<String>, ()> {
    match hash {
        None => Ok(None),
        Some(h) => store.load_blob(id, h).map(Some).map_err(|_| ()),
    }
}

/// 审查页上撤回了多少：一块都没撤回时为 None。
fn undo_of(page: &ReviewPage) -> Option<TurnUndo> {
    let (mut any, mut all) = (false, true);
    for f in &page.files {
        if let FileState::Ready { hunks, .. } = &f.state {
            let n = f.reverted();
            any |= n > 0;
            all &= n == hunks.len();
        }
    }
    any.then_some(TurnUndo { turn: page.turn, all, exact: true })
}

/// 磁盘上的现状对得上日志里最后写的样子。
fn matches_disk(path: &str, current: &Option<String>) -> bool {
    match checkpoint::read_snapshot(Path::new(path)) {
        Some(text) => text.as_deref().map(content_hash) == *current,
        None => false,
    }
}

impl ChatApp {
    /// 当前会话是不是开着审查页（且还对得上：同一会话、还是最新一轮、这一轮没在跑）。
    pub(super) fn review_open(&self) -> bool {
        self.review.as_ref().is_some_and(|p| {
            let s = self.session();
            p.token == s.token && !s.busy && s.latest_turn() == Some(p.turn)
        })
    }

    /// 打开最新一轮的审查页；`item` 是这一轮回复末尾那一行（改动卡片所在）。
    pub(super) fn open_review(&mut self, item: usize, cx: &mut Context<Self>) {
        match self.review_page(item) {
            Ok(page) => {
                self.review = Some(page);
                self.review_scroll = ScrollHandle::new();
                self.sync_undo();
                cx.notify();
            }
            Err(Some(msg)) => self.toast(msg, cx),
            Err(None) => {}
        }
    }

    /// 读日志和 blob，算出最新一轮的审查页（不打开）。算不了时给出要提示用户的话（None = 不用提示）。
    fn review_page(&mut self, item: usize) -> Result<ReviewPage, Option<&'static str>> {
        let active = self.active;
        let Some(turn) = self.sessions[active].latest_turn() else { return Err(None) };
        let prompt = self.sessions[active].transcript.iter().rev().find_map(|i| match i {
            ChatItem::User { text, .. } => Some(text.to_string()),
            _ => None,
        });
        let Some(records) = self.effective_records(active) else {
            return Err(Some("这个会话没有保存记录（演示回放不落盘），审查不了"));
        };
        // 对话里的这一轮要和日志里的对得上，否则宁可不做
        let logged = checkpoint::user_index(&records, turn).and_then(|i| match &records[i] {
            Record::User { text, .. } => Some(text.clone()),
            _ => None,
        });
        if logged.is_none() || logged != prompt {
            return Err(Some("对话和保存的记录对不上，审查不了"));
        }
        let (Some(store), Some(id)) =
            (self.store.clone(), self.sessions[active].meta.as_ref().map(|m| m.id.clone()))
        else {
            return Err(None);
        };
        let s = &self.sessions[active];
        let cwd = s.cwd.clone();
        let mut files: Vec<ReviewFile> = review::turn_review(&records, turn)
            .into_iter()
            .map(|r| Self::review_file(&store, &id, &cwd, r))
            .collect();
        // 对话里报了 diff、日志里却没有检查点的文件：只能看
        for f in s.turn_files(item) {
            let display = review::display_path(&f.path, Some(&cwd));
            if !files.iter().any(|r| same_path(&r.display, &display)) {
                files.push(ReviewFile {
                    display,
                    state: FileState::NoSnapshot { added: f.added, removed: f.removed },
                });
            }
        }
        let commands = checkpoint::commands_after(&records, turn);
        Ok(ReviewPage { token: s.token, turn, files, commands })
    }

    /// 切到一个重开的会话时：日志里只看得出最新一轮「撤回过」（`TurnUndo::exact` 为假），
    /// 这里对着 diff 数清是全撤回了还是一部分（不打开审查页、不提示）。
    pub(super) fn settle_undo(&mut self) {
        let s = self.session();
        if !s.undo.is_some_and(|u| !u.exact) {
            return;
        }
        let Some(item) = (0..s.transcript.len()).rev().find(|&i| s.is_turn_end(i)) else { return };
        let undo = self.review_page(item).ok().and_then(|page| undo_of(&page));
        let active = self.active;
        self.sessions[active].undo = undo;
    }

    /// 改动卡片上的「撤回」/「恢复」：不打开审查页，把这一轮能撤回的文件整个撤回（`undo`）或全部恢复。
    /// 走审查页同一套写盘和记日志（[`Self::set_reverted`]），所以之后在审查页里还能逐块改。
    pub(super) fn undo_turn(&mut self, item: usize, undo: bool, cx: &mut Context<Self>) {
        let was_open = self.review_open();
        self.open_review(item, cx);
        if !self.review_open() {
            return;
        }
        let n = self.review.as_ref().map_or(0, |p| p.files.len());
        for i in 0..n {
            self.revert_file(i, undo, cx);
        }
        let Some(page) = self.review.as_ref() else { return };
        let ready = page
            .files
            .iter()
            .filter(|f| matches!(&f.state, FileState::Ready { hunks, .. } if !hunks.is_empty()))
            .count();
        let stuck =
            page.files.iter().filter(|f| !matches!(f.state, FileState::Ready { .. })).count();
        let commands = page.commands;
        if !was_open {
            self.review = None;
        }
        let mut text = match (ready, undo) {
            (0, _) => "这一轮的改动没有检查点快照（或改完后又被改过），撤回不了".to_string(),
            (n, true) => format!("撤回了 {n} 个文件的改动，「审查」里可以逐块恢复"),
            (n, false) => format!("恢复了 {n} 个文件的改动"),
        };
        if ready > 0 && stuck > 0 {
            text.push_str(&format!("；{stuck} 个文件改完后又被改过或没有快照，没有动"));
        }
        if undo && commands > 0 {
            text.push_str("（命令改的文件不在内）");
        }
        self.toast(text, cx);
        cx.notify();
    }

    /// 按审查页的现状更新会话的撤回情况（改动卡片的表头和按钮跟着它，见 `Session::undo`）。
    fn sync_undo(&mut self) {
        let Some(page) = &self.review else { return };
        let undo = undo_of(page);
        let active = self.active;
        self.sessions[active].undo = undo;
    }

    /// 一个文件的审查状态：读 base / head，比对磁盘。
    fn review_file(
        store: &claudesktop_core::SessionStore,
        id: &str,
        cwd: &Path,
        review: FileReview,
    ) -> ReviewFile {
        let display = review::display_path(&review.path, Some(cwd));
        let (Ok(base), Ok(head)) = (load(store, id, &review.base), load(store, id, &review.head))
        else {
            return ReviewFile { display, state: FileState::NoSnapshot { added: 0, removed: 0 } };
        };
        let hunks = review::hunks(base.as_deref().unwrap_or(""), head.as_deref().unwrap_or(""));
        if !matches_disk(&review.path, &review.current) {
            let (added, removed) =
                hunks.iter().fold((0, 0), |(a, r), h| (a + h.added(), r + h.removed()));
            return ReviewFile { display, state: FileState::Conflict { added, removed } };
        }
        ReviewFile { display, state: FileState::Ready { review, base, head, hunks } }
    }

    pub(super) fn close_review(&mut self, cx: &mut Context<Self>) {
        self.review = None;
        cx.notify();
    }

    /// 把第 `file` 个文件的撤回状态改成 `reverted`：拼出内容、写盘、记日志。
    fn set_reverted(&mut self, file: usize, reverted: Vec<HunkRange>, cx: &mut Context<Self>) {
        if !self.review_open() {
            return;
        }
        let active = self.active;
        let (Some(store), Some(id)) =
            (self.store.clone(), self.sessions[active].meta.as_ref().map(|m| m.id.clone()))
        else {
            return;
        };
        let Some(page) = self.review.as_mut() else { return };
        let Some(f) = page.files.get_mut(file) else { return };
        let FileState::Ready { review, base, head, hunks } = &mut f.state else { return };
        // 打开页面之后又被改过：转成只读，不覆盖
        if !matches_disk(&review.path, &review.current) {
            let (added, removed) =
                hunks.iter().fold((0, 0), |(a, r), h| (a + h.added(), r + h.removed()));
            let name = file_name(&review.path);
            f.state = FileState::Conflict { added, removed };
            self.toast(format!("{name} 在 agent 改完后又被改过，没有动它"), cx);
            cx.notify();
            return;
        }
        let text = review::splice(base.as_deref(), head.as_deref(), &reverted);
        let path = Path::new(&review.path);
        let written = match &text {
            None => match std::fs::remove_file(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
            Some(text) => path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(path, text)),
        };
        if let Err(err) = written {
            let name = file_name(&review.path);
            self.toast(format!("没写上 {name}：{err}"), cx);
            return;
        }
        let after = match text.as_deref().map(|t| store.save_blob(&id, t)).transpose() {
            Ok(after) => after,
            Err(err) => {
                // 文件已经写了，只是检查点没存上：照样记下撤回状态，回退时这个文件对不上会当冲突
                self.sessions[active]
                    .note(format!("检查点没存上（回退时还原不了 {}）：{err}", review.path));
                text.as_deref().map(content_hash)
            }
        };
        let record = Record::FileChanged {
            path: review.path.clone(),
            before: review.current.clone(),
            after: after.clone(),
            reverted: Some(reverted.clone()),
        };
        review.current = after;
        review.reverted = reverted;
        self.ensure_log(active);
        self.persist(active, record);
        self.sessions[active].review_pending = true;
        self.sync_undo();
        cx.notify();
    }

    /// 撤回 / 恢复一块。
    fn toggle_hunk(&mut self, file: usize, range: HunkRange, cx: &mut Context<Self>) {
        let Some(FileState::Ready { review, .. }) =
            self.review.as_ref().and_then(|p| p.files.get(file)).map(|f| &f.state)
        else {
            return;
        };
        let mut set = review.reverted.clone();
        match set.iter().position(|r| *r == range) {
            Some(i) => drop(set.remove(i)),
            None => set.push(range),
        }
        self.set_reverted(file, set, cx);
    }

    /// 整个文件撤回（`all`）或全部恢复。
    fn revert_file(&mut self, file: usize, all: bool, cx: &mut Context<Self>) {
        let Some(FileState::Ready { hunks, .. }) =
            self.review.as_ref().and_then(|p| p.files.get(file)).map(|f| &f.state)
        else {
            return;
        };
        let set = if all { hunks.iter().map(|h| h.range).collect() } else { Vec::new() };
        self.set_reverted(file, set, cx);
    }

    /// 调试钩子 `CLAUDESKTOP_OPEN=review[:N]`：切到侧栏第 2 个会话、打开它最新一轮的审查页，
    /// 给了 N 时再把第 N 个文件的第一块撤回（看撤回后的样子）。
    pub(crate) fn debug_review(&mut self, file: Option<usize>, cx: &mut Context<Self>) {
        if self.sessions.len() < 2 {
            return;
        }
        self.activate_session(1, cx);
        let s = self.session();
        let Some(item) = (0..s.transcript.len()).rev().find(|&i| s.is_turn_end(i)) else { return };
        self.open_review(item, cx);
        let first = |p: &ReviewPage, n: usize| match p.files.get(n).map(|f| &f.state) {
            Some(FileState::Ready { hunks, .. }) => hunks.first().map(|h| h.range),
            _ => None,
        };
        if let Some(n) = file
            && let Some(range) = self.review.as_ref().and_then(|p| first(p, n))
        {
            self.toggle_hunk(n, range, cx);
        }
    }

    /// 发消息前：最新一轮撤回过块时，给这条消息附上的说明（只附一次）。
    pub(super) fn take_review_note(&mut self, idx: usize) -> Option<String> {
        if !std::mem::take(&mut self.sessions[idx].review_pending) {
            return None;
        }
        let records = self.effective_records(idx)?;
        let store = self.store.clone()?;
        let id = self.sessions[idx].meta.as_ref()?.id.clone();
        let cwd = self.sessions[idx].cwd.clone();
        review::review_note(&records, Some(&cwd), |hash| store.load_blob(&id, hash).ok())
    }

    /// 审查页：标题、文件一个接一个，每个文件下面是它的块。
    pub(super) fn render_review_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(page) = &self.review else { return div().into_any_element() };
        let reverted: usize = page.files.iter().map(ReviewFile::reverted).sum();
        let summary = if reverted == 0 {
            format!("{} 个文件 · 都保留着", page.files.len())
        } else {
            format!("{} 个文件 · 撤回了 {reverted} 块，下一条消息会告诉 agent", page.files.len())
        };
        let header = div()
            .h_flex()
            .items_start()
            .gap_2()
            .child(
                Self::quiet_button("review-back", IconAll::ArrowLeft, Surface::Paper, cx)
                    .small()
                    .tooltip("返回对话")
                    .on_click(cx.listener(|this, _, _, cx| this.close_review(cx))),
            )
            .child(
                div()
                    .v_flex()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_size(TEXT_LEAD)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(INK))
                            .child("审查这一轮的改动"),
                    )
                    .child(
                        div()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .font_features(tabular_nums())
                            .child(summary),
                    ),
            );
        let mut body = div().v_flex().gap_6().child(header);
        for (i, f) in page.files.iter().enumerate() {
            body = body.child(self.render_review_file(i, f, cx));
        }
        if page.commands > 0 {
            body = body.child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_1p5()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(Icon::new(IconAll::Info).size_3p5().mt_0p5())
                    .child(format!("这一轮跑过 {} 条命令，命令改动的文件不在这里", page.commands)),
            );
        }
        div()
            .id("review-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.review_scroll)
            .child(div().max_w(READING_W).mx_auto().w_full().px_6().py_6().child(body))
            .into_any_element()
    }

    /// 一个文件：表头（路径、增删、整文件操作），下面是块；只读的写明原因。
    fn render_review_file(&self, i: usize, f: &ReviewFile, cx: &mut Context<Self>) -> Div {
        let (added, removed) = f.stat();
        let reverted = f.reverted();
        let mut head = div()
            .h_flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(Icon::new(IconAll::FileText).size_3p5().text_color(rgb(INK_SOFT)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK))
                    .child(f.display.clone()),
            )
            .child(diff_stat(added, removed));
        let mut out = div().v_flex().gap_2();
        let note =
            |text: &'static str| div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(text);
        match &f.state {
            FileState::NoSnapshot { .. } => out.child(head).child(note(
                "没有改动前的快照（文件太大、不是文本，或 agent 自己直接写的盘），只能在对话里查看",
            )),
            FileState::Conflict { .. } => out.child(head).child(note(
                "这个文件在 agent 改完后又被改过，没法逐块撤回；要整轮还原请用用户消息下的「回退」",
            )),
            FileState::Ready { review, hunks, .. } => {
                let all = reverted == hunks.len();
                head = head
                    .when(reverted > 0, |d| {
                        d.child(
                            Self::message_action(
                                ("review-restore-all", i),
                                IconAll::Redo2,
                                "全部恢复",
                                cx,
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.revert_file(i, false, cx)),
                            ),
                        )
                    })
                    .when(!all, |d| {
                        d.child(
                            Self::message_action(
                                ("review-revert-all", i),
                                IconAll::Undo2,
                                "撤回整个文件",
                                cx,
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.revert_file(i, true, cx)),
                            ),
                        )
                    });
                out = out.child(head);
                for (k, h) in hunks.iter().enumerate() {
                    let off = review.reverted.contains(&h.range);
                    out = out.child(Self::render_hunk(i, k, h, off, cx));
                }
                out
            }
        }
    }

    /// 一块：右上角撤回 / 恢复；撤回了的整块淡下去。
    fn render_hunk(file: usize, k: usize, h: &Hunk, off: bool, cx: &mut Context<Self>) -> Div {
        let range = h.range;
        let (label, icon) =
            if off { ("恢复", IconAll::Redo2) } else { ("撤回", IconAll::Undo2) };
        let shown = h.lines.len().min(HUNK_LINES_MAX);
        let bar = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .bg(rgb(SAND))
            .border_b_1()
            .border_color(rgb(LINE))
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .font_features(tabular_nums())
            .child(div().flex_1().min_w_0().child(if off {
                "已撤回：磁盘上是改动前的内容".to_string()
            } else {
                format!("第 {} 行", range.new_start + 1)
            }))
            .child(
                // 卡片头是沙色底：透明底按钮悬停出 `SELECTED`
                Self::quiet_button(("review-hunk", file * 100_000 + k), icon, Surface::Sand, cx)
                    .xsmall()
                    .label(label)
                    .px_2()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_hunk(file, range, cx))),
            );
        div()
            .v_flex()
            .rounded_lg()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .overflow_hidden()
            .child(bar)
            .child(
                div()
                    .v_flex()
                    .py_1()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .line_height(LEADING)
                    .when(off, |d| d.opacity(0.45))
                    .children(h.lines[..shown].iter().map(diff_line))
                    .when(h.lines.len() > shown, |d| {
                        d.child(
                            div()
                                .px_3()
                                .py_1()
                                .font_family(sans())
                                .text_color(rgb(INK_SOFT))
                                .child(format!("还有 {} 行未显示", h.lines.len() - shown)),
                        )
                    }),
            )
    }
}
