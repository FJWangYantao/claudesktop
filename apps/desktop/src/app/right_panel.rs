//! 右侧辅助面板：文件树 / 源码预览与逐文件 Diff。
//!
//! 清单和预览只构造视口附近的行。目录在面板绑定会话、刷新或首次展开时读取。
//! 展开为全宽时隐藏对话列；窄面板一次显示清单或预览，宽面板并排显示。

use super::ChatApp;
use super::chat::{diff_line, diff_truncated};
use super::chrome::CHROME_H;
use super::menu::diff_stat;
use super::widgets::Surface;
use crate::model::{DiffLocation, TurnFile, same_path, turn_files};
use crate::session::Session;
use crate::settings::ContentAlign;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::DiffView;
use claudesktop_core::UserPrompt;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

const PREVIEW_LIMIT: usize = 256 * 1024;
const MAX_DEPTH: usize = 8;
const FILE_ROW_H: Rems = rems(1.75);

/// 右侧那一列停在哪一类 Pane 上（顶栏开关 / 快捷键 / 命令面板都走这个）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RightTab {
    #[default]
    Files,
    Changes,
}

impl RightTab {
    /// 对应的 Pane 种类：Tab 的标题和图标由 `PaneKind` 给，不再各写一份。
    pub(super) fn pane_kind(self) -> crate::app::pane::PaneKind {
        match self {
            Self::Files => crate::app::pane::PaneKind::Files,
            Self::Changes => crate::app::pane::PaneKind::Diff,
        }
    }
}

struct FileNode {
    name: SharedString,
    path: PathBuf,
    directory: bool,
}

#[derive(Clone, Copy)]
struct FileRow {
    directory: usize,
    item: usize,
    depth: usize,
}

#[derive(Default)]
struct FileTree {
    directory_ids: HashMap<PathBuf, usize>,
    directories: Vec<Vec<FileNode>>,
    expanded: HashSet<PathBuf>,
    rows: Vec<FileRow>,
    query: String,
}

impl FileTree {
    fn load_dir(&mut self, dir: &Path) {
        if self.directory_ids.contains_key(dir) {
            return;
        }
        let mut nodes: Vec<FileNode> = std::fs::read_dir(dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let kind = entry.file_type().ok()?;
                Some(FileNode {
                    name: entry.file_name().to_string_lossy().into_owned().into(),
                    directory: kind.is_dir(),
                    path: entry.path(),
                })
            })
            .collect();
        nodes.sort_by_key(|node| (!node.directory, node.name.to_string().to_lowercase()));
        self.directory_ids.insert(dir.to_path_buf(), self.directories.len());
        self.directories.push(nodes);
    }

    fn rebuild(&mut self, root: &Path) {
        let mut rows = std::mem::take(&mut self.rows);
        rows.clear();
        self.append_rows(root, 0, &mut rows);
        self.rows = rows;
    }

    fn append_rows(&self, dir: &Path, depth: usize, rows: &mut Vec<FileRow>) {
        if depth > MAX_DEPTH {
            return;
        }
        let Some(&directory) = self.directory_ids.get(dir) else { return };
        for item in 0..self.directories[directory].len() {
            let node = &self.directories[directory][item];
            if !self.query.is_empty() && !node.name.to_lowercase().contains(&self.query) {
                continue;
            }
            rows.push(FileRow { directory, item, depth });
            if node.directory && self.expanded.contains(&node.path) {
                self.append_rows(&node.path, depth + 1, rows);
            }
        }
    }

    fn node(&self, row: FileRow) -> &FileNode {
        &self.directories[row.directory][row.item]
    }

    fn position(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|&row| self.node(row).path == path)
    }

    fn is_empty_dir(&self, dir: &Path) -> bool {
        self.directory_ids.get(dir).is_some_and(|&id| self.directories[id].is_empty())
    }
}

#[derive(Default)]
struct FilePreview {
    path: PathBuf,
    text: String,
    line_starts: Vec<u32>,
    truncated: bool,
    error: Option<String>,
    list: Option<ListState>,
    scroll_top: ListOffset,
}

impl FilePreview {
    fn set_text(&mut self, text: String) {
        let mut start = 0;
        self.line_starts = text
            .split_inclusive('\n')
            .map(|line| {
                let offset = start;
                start += line.len();
                offset as u32
            })
            .collect();
        self.text = text;
    }

    fn line(&self, index: usize) -> Option<&str> {
        let start = *self.line_starts.get(index)? as usize;
        let end = self.line_starts.get(index + 1).map_or(self.text.len(), |&n| n as usize);
        let raw = &self.text[start..end];
        Some(raw.strip_suffix('\n').map_or(raw, |line| line.strip_suffix('\r').unwrap_or(line)))
    }

    fn list(&mut self, rem: Pixels) -> ListState {
        let count = self.line_starts.len() + usize::from(self.truncated);
        self.list
            .get_or_insert_with(|| {
                let list = ListState::new(count, ListAlignment::Top, rems(8.).to_pixels(rem));
                list.scroll_to(self.scroll_top);
                list
            })
            .clone()
    }

    fn release_list(&mut self) {
        if let Some(list) = self.list.take() {
            self.scroll_top = list.logical_scroll_top();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DiffSelection {
    location: DiffLocation,
    epoch: u64,
}

impl DiffSelection {
    fn resolve(self, session: &Session) -> Option<&DiffView> {
        if self.epoch != session.diff_epoch {
            return None;
        }
        self.location.resolve(&session.transcript)
    }
}

struct DiffPreview {
    selection: DiffSelection,
    list: Option<ListState>,
    scroll_top: ListOffset,
    revision: u64,
}

impl DiffPreview {
    fn new(selection: DiffSelection) -> Self {
        Self { selection, list: None, scroll_top: ListOffset::default(), revision: 0 }
    }

    fn list(&mut self, count: usize, revision: u64, rem: Pixels) -> ListState {
        if let Some(list) = &self.list
            && list.item_count() == count
        {
            if self.revision != revision {
                list.remeasure();
                self.revision = revision;
            }
            return list.clone();
        }
        self.release_list();
        let list = ListState::new(count, ListAlignment::Top, rems(8.).to_pixels(rem));
        list.scroll_to(self.scroll_top);
        self.revision = revision;
        self.list = Some(list.clone());
        list
    }

    fn release_list(&mut self) {
        if let Some(list) = self.list.take() {
            self.scroll_top = list.logical_scroll_top();
        }
    }
}

struct ChangedFiles {
    revision: u64,
    /// 从哪条记录往后算（本轮 vs 整个会话）
    offset: usize,
    rows: Vec<TurnFile>,
}

impl ChangedFiles {
    fn sync(cache: &mut Option<Self>, session: &Session, offset: usize) {
        let stale = cache
            .as_ref()
            .is_none_or(|files| files.revision != session.diff_revision || files.offset != offset);
        if stale {
            *cache = Some(Self {
                revision: session.diff_revision,
                offset,
                rows: turn_files(&session.transcript, offset),
            });
        }
    }
}

/// 改动 Pane 顶部的取数范围。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DiffScope {
    /// 本轮（工具上报的 diffs，可逐块撤回）
    #[default]
    Turn,
    /// 这个会话改过的全部文件
    Session,
    /// git 未提交（staged + unstaged）
    WorkingTree,
    /// 相对基线分支（`<base>...HEAD`）
    BranchBase,
}

impl DiffScope {
    pub(crate) const ALL: [DiffScope; 4] =
        [DiffScope::Turn, DiffScope::Session, DiffScope::WorkingTree, DiffScope::BranchBase];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Turn => "本轮",
            Self::Session => "会话",
            Self::WorkingTree => "未提交",
            Self::BranchBase => "基线",
        }
    }

    fn git(self) -> bool {
        matches!(self, Self::WorkingTree | Self::BranchBase)
    }

    /// 从哪条记录开始算：本轮从最后一条用户消息算（和 `Session::turn_files` 同一口径），
    /// 会话从 0 算（= 整个 transcript）
    fn start(self, session: &Session) -> usize {
        match self {
            Self::Turn => session
                .transcript
                .iter()
                .rposition(|item| matches!(item, crate::model::ChatItem::User { .. }))
                .unwrap_or(0),
            _ => 0,
        }
    }
}

/// git diff 解析出来的一行：文件头 / 正文行 / 说明
pub(super) enum GitRow {
    File {
        path: String,
        added: u32,
        removed: u32,
    },
    /// 正文行带上它所属的文件（行内批注要按 `(path, line)` 定位）
    Line {
        path: String,
        line: claudesktop_core::DiffLineView,
    },
    Note(SharedString),
}

/// 一条行内批注（哪一行、什么上下文、用户写了什么）
#[derive(Clone, Debug)]
pub(crate) struct DiffComment {
    /// 相对工作目录的路径
    pub(crate) path: String,
    /// 行号（新增行是新文件行号，删除行是旧文件行号）
    pub(crate) line: u32,
    /// `'+'` / `'-'` / `' '`
    pub(crate) tag: char,
    /// 那一行的内容（发送时给模型当上下文）
    pub(crate) snippet: String,
    pub(crate) text: String,
}

/// 正在写批注的那一行（点了行上的 `+` 之后）
#[derive(Clone, Debug)]
pub(crate) struct CommentTarget {
    /// 点这一行时是哪个会话：切会话之后这条目标就作废（不然会写进另一个会话）
    pub(crate) token: u64,
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) tag: char,
    pub(crate) snippet: String,
}

impl CommentTarget {
    /// 一句话描述：`src/a.rs:42 +`
    pub(crate) fn label(&self) -> String {
        format!("{}:{} {}", self.path, self.line, self.tag)
    }
}

/// 批注发出去时的 `<user-review>` 块。
///
/// 和 `review::review_note`（逐块撤回的说明）是同一套约定：都要求模型**别重做**这些改动，
/// 只按批注调。两条说明会一起拼在用户这条消息前面（见 `ChatApp::submit_to` 的注入点）。
pub(crate) fn comments_note(comments: &[DiffComment]) -> Option<String> {
    if comments.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(comments.len());
    for comment in comments {
        let tag = match comment.tag {
            '+' => "added",
            '-' => "removed",
            _ => "context",
        };
        let text = comment.text.trim();
        if text.is_empty() {
            continue;
        }
        lines.push(format!(
            "- {}:{} ({tag}) `{}`\n  {}",
            comment.path,
            comment.line,
            comment.snippet.trim(),
            text
        ));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "<user-review>\nThe user reviewed the diff and left comments on specific lines \
         (file:line (kind) `code`, then the comment):\n{}\nAddress these comments; do not redo \
         changes the user did not ask about.\n</user-review>",
        lines.join("\n")
    ))
}

/// git 两档的取数结果。
///
/// 三件事必须一起做到：**键带会话令牌**（否则切到另一个恰好同 `diff_revision` 的项目会话
/// 会继续显示上一个仓库的改动）、**git 子进程不进 render**（Windows 上一次能占几百毫秒）、
/// **有手动刷新**（用户手改文件不会递增 `diff_revision`）。
#[derive(Default)]
struct GitDiff {
    /// 已经装好结果的键：(会话令牌, diff_revision, 范围)
    loaded: Option<(u64, u64, DiffScope)>,
    /// 正在算的键（同一份请求不重复发）
    pending: Option<(u64, u64, DiffScope)>,
    rows: Vec<GitRow>,
    error: Option<String>,
    list: Option<ListState>,
    scroll_top: ListOffset,
}

/// 一份 git 取数请求（进背景线程用，全是 owned 数据）
pub(super) struct GitDiffRequest {
    pub(super) scope: DiffScope,
    pub(super) dir: PathBuf,
    /// worktree 会话的基线（`WorktreeMeta::base`）：有它就一定用它，
    /// 普通仓库的基线在后台线程里现算（见 `compute_git_diff`）
    pub(super) worktree_base: Option<String>,
}

impl GitDiff {
    fn key_of(session: &Session, scope: DiffScope) -> (u64, u64, DiffScope) {
        (session.token, session.diff_revision, scope)
    }

    /// 结果装回来；期间用户换了会话 / 范围就丢掉这份迟到的结果。
    fn install(&mut self, key: (u64, u64, DiffScope), rows: Vec<GitRow>, error: Option<String>) {
        if self.pending != Some(key) {
            return;
        }
        self.pending = None;
        self.loaded = Some(key);
        self.rows = rows;
        self.error = error;
        self.list = None;
    }

    /// 切会话 / 手动刷新时整份作废。
    fn invalidate(&mut self) {
        self.loaded = None;
        self.pending = None;
        self.rows.clear();
        self.error = None;
        self.list = None;
    }

    fn list(&mut self, rem: Pixels) -> ListState {
        let count = self.rows.len();
        self.list
            .get_or_insert_with(|| {
                let list = ListState::new(count, ListAlignment::Top, rems(8.).to_pixels(rem));
                list.scroll_to(self.scroll_top);
                list
            })
            .clone()
    }

    fn release_list(&mut self) {
        if let Some(list) = self.list.take() {
            self.scroll_top = list.logical_scroll_top();
        }
    }
}

/// 背景线程里跑的那一段：把 git 输出翻成可以直接画的行。
pub(super) fn compute_git_diff(request: &GitDiffRequest) -> (Vec<GitRow>, Option<String>) {
    let dir = request.dir.as_path();
    if claudesktop_core::git::repo_root(dir).is_none() {
        return (Vec::new(), Some("这个目录不是 git 仓库：只有「本轮 / 会话」两档可用".into()));
    }
    let text = match request.scope {
        DiffScope::WorkingTree => claudesktop_core::git::working_tree_diff(dir),
        // 基线在这里现算：`default_base` 要起好几次 git 子进程，不能放在 UI 线程
        DiffScope::BranchBase => {
            let base =
                request.worktree_base.clone().or_else(|| claudesktop_core::git::default_base(dir));
            base.and_then(|base| claudesktop_core::git::branch_diff(dir, &base))
        }
        _ => None,
    };
    let Some(text) = text else {
        let message = match request.scope {
            DiffScope::BranchBase => "当前分支没有可对比的基线分支",
            _ => "读不到 git 的改动",
        };
        return (Vec::new(), Some(message.into()));
    };
    let mut rows = parse_unified_diff(&text);
    // 未跟踪的文件没有 base 版本可以 diff，只列出来——只属于「未提交」这一档
    if request.scope == DiffScope::WorkingTree {
        let untracked = claudesktop_core::git::untracked_files(dir);
        if !untracked.is_empty() {
            rows.push(GitRow::Note(
                format!("另有 {} 个未跟踪文件（没有可比较的旧版本）", untracked.len()).into(),
            ));
            for path in untracked.iter().take(20) {
                rows.push(GitRow::Note(
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                        .into(),
                ));
            }
        }
    }
    if rows.is_empty() {
        rows.push(GitRow::Note("工作区是干净的".into()));
    }
    (rows, None)
}

/// 把 `git diff` 的文本拆成可以逐行画的形状（文件头 + 正文行 + hunk 分隔）。
///
/// 只认统一格式的几种行：`diff --git` / `---` / `+++` / `@@` / `+` / `-` / 空格开头。
/// 其余（`index`、`similarity index`、`\ No newline` 之类）丢掉——它们在面板里没有意义。
fn parse_unified_diff(text: &str) -> Vec<GitRow> {
    let mut rows = Vec::new();
    // 当前文件：路径、正文行、+/- 统计（统计要整段读完才知道，所以文件头最后补在最前面）
    let mut path: Option<String> = None;
    let mut body: Vec<GitRow> = Vec::new();
    let (mut added, mut removed) = (0u32, 0u32);
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    // 是否已经进了 hunk 正文：`---` / `+++` 在 hunk 里是**真的代码行**，不能按头部吞掉
    let mut in_hunk = false;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush_file(&mut rows, &mut path, &mut body, &mut added, &mut removed);
            let _ = rest;
            // `diff --git a/X b/X`：取 ` b/` 后面那段
            path = Some(line.rsplit(" b/").next().unwrap_or_default().to_string());
            in_hunk = false;
            continue;
        }
        if path.is_none() {
            continue;
        }
        if line.starts_with("@@") {
            in_hunk = true;
            // `@@ -a,b +c,d @@`：解析两边的起始行号
            let mut parts = line.trim_start_matches("@@ -").split(' ');
            if let Some(old) = parts.next() {
                old_no = old.split(',').next().and_then(|n| n.parse().ok()).unwrap_or(0);
            }
            if let Some(new) = parts.next() {
                new_no = new
                    .strip_prefix('+')
                    .and_then(|n| n.split(',').next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
            }
            body.push(GitRow::Line {
                path: path.clone().unwrap_or_default(),
                line: claudesktop_core::DiffLineView { tag: '~', no: 0, text: line.to_string() },
            });
            continue;
        }
        if !in_hunk && (line.starts_with("+++") || line.starts_with("---")) {
            // 文件头里的路径行：`diff --git` 已经给了，跳过。
            // **只在 hunk 之外判**——Markdown 的 `---`、SQL 的 `-- 注释`、C++ 的 `++i`
            // 在正文里会变成 `---...` / `+++...`，按前缀吞掉会把真实代码行弄丢。
            continue;
        }
        if line.starts_with('\\') {
            // `\ No newline at end of file`
            continue;
        }
        // 只有上下文行（空格开头）、增行、删行才是正文；`index` / `new file mode` /
        // `similarity index` / `Binary files` 这些都是 git 的元信息，画出来只会干扰
        if !matches!(line.chars().next(), Some('+' | '-' | ' ')) {
            continue;
        }
        let (tag, text, no) = if let Some(rest) = line.strip_prefix('+') {
            added += 1;
            let no = new_no;
            new_no += 1;
            ('+', rest, no)
        } else if let Some(rest) = line.strip_prefix('-') {
            removed += 1;
            let no = old_no;
            old_no += 1;
            ('-', rest, no)
        } else {
            let rest = line.strip_prefix(' ').unwrap_or(line);
            let no = new_no;
            old_no += 1;
            new_no += 1;
            (' ', rest, no)
        };
        body.push(GitRow::Line {
            path: path.clone().unwrap_or_default(),
            line: claudesktop_core::DiffLineView { tag, no, text: text.to_string() },
        });
    }
    flush_file(&mut rows, &mut path, &mut body, &mut added, &mut removed);
    rows
}

/// 收尾一个文件：先写它的文件头，再跟它那一整段正文。
fn flush_file(
    rows: &mut Vec<GitRow>,
    path: &mut Option<String>,
    body: &mut Vec<GitRow>,
    added: &mut u32,
    removed: &mut u32,
) {
    let Some(path) = path.take() else { return };
    rows.push(GitRow::File {
        path,
        added: std::mem::take(added),
        removed: std::mem::take(removed),
    });
    rows.append(body);
}

pub(super) struct RightPanelState {
    pub open: bool,
    pub tab: RightTab,
    seen_token: Option<u64>,
    root: PathBuf,
    file_tree: FileTree,
    pub(crate) selected_file: Option<PathBuf>,
    preview: Option<FilePreview>,
    diff_preview: Option<DiffPreview>,
    changed_files: Option<ChangedFiles>,
    filter_input: Entity<InputState>,
    file_scroll: UniformListScrollHandle,
    diff_list_scroll: UniformListScrollHandle,
    /// 这一帧这个面板够不够宽、要不要并排双栏（由各 Pane 自己量的宽度算，见 `render_files_tab`）
    split: bool,
    /// 本轮改动文件数（改动 Pane 的 Tab badge 用，按 `(会话令牌, diff_revision)` 缓存）
    pub(crate) changed_count: usize,
    changed_count_key: Option<(u64, u64)>,
    /// 改动 Pane 顶部选的范围
    pub(crate) scope: DiffScope,
    /// git 两档的取数结果
    git: GitDiff,
}

/// 行内批注的状态（按会话令牌归属：切会话不会把 A 的批注发进 B）
#[derive(Default)]
pub(crate) struct CommentDraft {
    pub(crate) by_session: HashMap<u64, Vec<DiffComment>>,
    /// 正在写的那一行
    pub(crate) target: Option<CommentTarget>,
    /// 输入框绑定的窗口；弹出到别的窗时要重建（见 `on_window_changed`）
    pub(crate) input: Option<Entity<InputState>>,
}

impl CommentDraft {
    pub(crate) fn of(&self, token: u64) -> &[DiffComment] {
        self.by_session.get(&token).map_or(&[], |list| list.as_slice())
    }

    pub(crate) fn push(&mut self, token: u64, comment: DiffComment) {
        self.by_session.entry(token).or_default().push(comment);
    }

    pub(crate) fn clear(&mut self, token: u64) {
        self.by_session.remove(&token);
    }
}

impl RightPanelState {
    pub fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("筛选文件"));
        cx.subscribe_in(
            &filter_input,
            window,
            |this: &mut ChatApp, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = this.right_panel.filter_input.read(cx).value();
                    let panel = &mut this.right_panel;
                    let query = query.trim().to_lowercase();
                    if panel.file_tree.query != query {
                        panel.file_tree.query = query;
                        panel.rebuild_file_rows(window.rem_size(), None);
                        cx.notify();
                    }
                }
            },
        )
        .detach();
        Self {
            open: false,
            tab: RightTab::Files,
            seen_token: None,
            root: PathBuf::new(),
            file_tree: FileTree::default(),
            selected_file: None,
            preview: None,
            diff_preview: None,
            changed_files: None,
            filter_input,
            file_scroll: UniformListScrollHandle::new(),
            diff_list_scroll: UniformListScrollHandle::new(),
            split: false,
            changed_count: 0,
            changed_count_key: None,
            scope: DiffScope::default(),
            git: GitDiff::default(),
        }
    }

    fn sync_session(&mut self, token: u64, cwd: &Path) {
        if self.seen_token == Some(token) && self.root == cwd {
            return;
        }
        self.seen_token = Some(token);
        self.root = cwd.to_path_buf();
        self.refresh();
    }

    pub(super) fn refresh(&mut self) {
        self.file_tree =
            FileTree { query: std::mem::take(&mut self.file_tree.query), ..FileTree::default() };
        self.selected_file = None;
        self.preview = None;
        self.diff_preview = None;
        self.changed_files = None;
        // git 两档也整份作废：用户手改 / shell 改动不会递增 `diff_revision`
        self.git.invalidate();
        self.diff_list_scroll.0.borrow().base_handle.set_offset(point(px(0.), px(0.)));
        self.file_scroll.0.borrow().base_handle.set_offset(point(px(0.), px(0.)));
        self.file_tree.load_dir(&self.root);
        self.file_tree.rebuild(&self.root);
    }

    fn rebuild_file_rows(&mut self, rem_size: Pixels, collapsed: Option<&Path>) {
        let scroll = self.file_scroll.0.borrow().base_handle.clone();
        let row_height = FILE_ROW_H.to_pixels(rem_size);
        let row_offset = (-scroll.offset().y / row_height).max(0.);
        let anchor = if row_offset > 0. {
            self.file_tree
                .rows
                .get(row_offset as usize)
                .map(|&row| self.file_tree.node(row).path.clone())
        } else {
            None
        };
        self.file_tree.rebuild(&self.root);
        let offset = anchor
            .as_deref()
            .and_then(|path| self.file_tree.position(path))
            .map(|index| index as f32 + row_offset.fract())
            .or_else(|| {
                collapsed.and_then(|path| self.file_tree.position(path)).map(|index| index as f32)
            })
            .unwrap_or(0.);
        scroll.set_offset(point(px(0.), -row_height * offset));
    }

    fn toggle_entry(&mut self, path: PathBuf, directory: bool, rem_size: Pixels) {
        if directory {
            if self.file_tree.expanded.remove(&path) {
                self.rebuild_file_rows(rem_size, Some(&path));
            } else {
                self.file_tree.load_dir(&path);
                self.file_tree.expanded.insert(path);
                self.rebuild_file_rows(rem_size, None);
            }
        } else {
            self.open_file(path);
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        let preview = read_preview(&path);
        self.selected_file = Some(path);
        self.preview = Some(preview);
    }
}

fn read_preview(path: &Path) -> FilePreview {
    let mut result = FilePreview { path: path.to_path_buf(), ..FilePreview::default() };
    let read = (|| -> std::io::Result<()> {
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take((PREVIEW_LIMIT + 1) as u64).read_to_end(&mut bytes)?;
        result.truncated = bytes.len() > PREVIEW_LIMIT;
        bytes.truncate(PREVIEW_LIMIT);
        if bytes.contains(&0) {
            result.error = Some("这是二进制文件，不能按文本预览".into());
        } else {
            let text = String::from_utf8(bytes)
                .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
            result.set_text(text);
        }
        Ok(())
    })();
    if let Err(error) = read {
        result.error = Some(error.to_string());
    }
    result
}

#[derive(Clone, Copy)]
pub(super) struct PanelResize {
    pub start_x: Pixels,
    pub start_width: f32,
    pub right: bool,
}

impl ChatApp {
    pub(super) fn sidebar_width_rem(&self) -> f32 {
        (SIDEBAR_W.0 + self.sidebar_width_delta).clamp(13.75, 30.)
    }

    pub(super) fn sidebar_width(&self) -> Rems {
        rems(self.sidebar_width_rem())
    }

    pub(super) fn reading_column(&self, content: Div) -> Div {
        let content = content
            .max_w(if self.settings.wide_reading { READING_WIDE_W } else { READING_W })
            .w_full()
            .px_8();
        if self.settings.content_align == ContentAlign::Center {
            content.mx_auto()
        } else {
            content
        }
    }

    pub(super) fn right_panel_controls(&self) -> bool {
        self.right_panel.open && !self.settings_open && !self.session().is_chat()
    }

    pub(super) fn toggle_right_panel(
        &mut self,
        tab: RightTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.right_panel.open && self.right_panel.tab == tab {
            self.right_panel.open = false;
        } else {
            self.settings_open = false;
            self.right_panel.open = true;
            self.right_panel.tab = tab;
        }
        self.sync_right_dock(window, cx);
        self.focus_pane_of_kind(tab.pane_kind(), window, cx);
        cx.notify();
    }

    /// 顶栏单开关：开着就收起（并退出全宽），关着就打开上次停的 Tab。
    /// 指定 Tab 的入口仍走 `toggle_right_panel`（`Ctrl+Shift+F` / `Ctrl+Shift+D` / 命令面板）。
    pub(super) fn toggle_right_panel_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.right_panel.open {
            self.right_panel.open = false;
        } else {
            self.settings_open = false;
            self.right_panel.open = true;
        }
        self.sync_right_dock(window, cx);
        self.focus_pane_of_kind(self.right_panel.tab.pane_kind(), window, cx);
        cx.notify();
    }

    pub(super) fn open_right_diff(
        &mut self,
        location: DiffLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = &self.sessions[self.active];
        let selection = DiffSelection { location, epoch: session.diff_epoch };
        if selection.resolve(session).is_none() {
            return;
        }
        self.settings_open = false;
        let panel = &mut self.right_panel;
        panel.sync_session(session.token, &session.cwd);
        panel.open = true;
        panel.tab = RightTab::Changes;
        if panel.diff_preview.as_ref().is_none_or(|preview| preview.selection != selection) {
            panel.diff_preview = Some(DiffPreview::new(selection));
        }
        self.sync_right_dock(window, cx);
        self.focus_pane_of_kind(RightTab::Changes.pane_kind(), window, cx);
        cx.notify();
    }

    pub(super) fn render_sidebar_handle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("sidebar-resize")
            .w(px(6.))
            .h_full()
            .flex_shrink_0()
            .cursor_ew_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.panel_resizing = Some(PanelResize {
                        start_x: event.position.x,
                        start_width: this.sidebar_width_rem(),
                        right: false,
                    });
                    window.prevent_default();
                    cx.stop_propagation();
                }),
            )
    }

    /// 文件 Pane 的正文（外壳是 dock 的 Tab 栏，这里不再画自己的顶栏）。
    /// `width_rem` 是这个面板**实测**宽度（rem），双栏阈值按它判——以前读的是那个
    /// 已经废弃的固定宽度，结果拖宽 / Zoom / 弹成 900px 独立窗都进不了双栏。
    pub(super) fn render_files_tab(
        &mut self,
        width_rem: Option<f32>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.right_panel.split = width_rem.is_some_and(|w| w >= 36.);
        let session = &self.sessions[self.active];
        self.right_panel.sync_session(session.token, &session.cwd);
        let showing_preview = self.panel_split() || self.right_panel.selected_file.is_some();
        if !showing_preview && let Some(preview) = &mut self.right_panel.preview {
            preview.release_list();
        }
        self.refresh_changed_file_count();
        self.render_file_workspace(window, cx)
    }

    /// 改动 Pane 的正文：按顶部选的范围取数（本轮 / 会话走记录里的 diffs，
    /// 未提交 / 基线走 git）。
    pub(super) fn render_diff_tab(
        &mut self,
        width_rem: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.right_panel.split = width_rem.is_some_and(|w| w >= 36.);
        let body = self.render_diff_body(&*window, cx);
        // 批注底栏固定在面板底部（不放进虚拟列表，见 `render_comment_bar`）
        let bar = self.render_comment_bar(window, cx);
        v_flex()
            .size_full()
            .min_h_0()
            .child(body)
            .when_some(bar, |d, bar| d.child(bar))
            .into_any_element()
    }

    fn render_diff_body(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let session = &self.sessions[self.active];
        self.right_panel.sync_session(session.token, &session.cwd);
        self.refresh_changed_file_count();
        if self.right_panel.scope.git() {
            let scope = self.right_panel.scope;
            let key = {
                let session = &self.sessions[self.active];
                GitDiff::key_of(session, scope)
            };
            if self.right_panel.git.loaded != Some(key) {
                // 结果还没到（第一次进这一档 / 刚换会话）：请求一次并给个占位，
                // **不在 render 里跑 git 子进程**（Windows 上一次能占几百毫秒）
                self.request_git_diff(cx);
                return panel_note("正在读取 git 改动…").into_any_element();
            }
            return self.render_git_diff(window, cx);
        }
        // 审查页只在内容区那一处画（`content.rs`）：这里不再画第二份——
        // 两份会共用 `review_scroll`、元素 id 完全相同，还会互相抢滚动句柄。
        // 这个 Pane 只提供入口（Tab 栏的「逐块撤回」）。
        self.render_diff_workspace(window, cx)
    }

    /// git 两档的正文：一个可滚动的行列表（文件头 + diff 正文）。
    fn render_git_diff(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let rem = window.rem_size();
        let panel = &mut self.right_panel;
        if let Some(error) = &panel.git.error {
            return panel_note(error.clone()).into_any_element();
        }
        let font_size = TEXT_SMALL.to_pixels(rem);
        let row_height = LEADING.to_pixels(font_size.into(), rem);
        list(
            panel.git.list(rem),
            cx.processor(move |this, index, _window, cx| {
                // 先把这一行要用的东西拷出来：下面要 `&mut this` 建监听器
                let file = match this.right_panel.git.rows.get(index) {
                    Some(GitRow::File { path, added, removed }) => {
                        Some((path.clone(), *added, *removed))
                    }
                    _ => None,
                };
                if let Some((path, added, removed)) = file {
                    return div()
                        .h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .h(rems(2.))
                        .px_3()
                        .bg(rgb(SAND))
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
                                .child(path),
                        )
                        .child(diff_stat(added, removed))
                        .into_any_element();
                }
                let note = match this.right_panel.git.rows.get(index) {
                    Some(GitRow::Note(text)) => Some(text.clone()),
                    _ => None,
                };
                if let Some(text) = note {
                    return panel_note(text).px_3().py_1().into_any_element();
                }
                let Some((path, line)) = (match this.right_panel.git.rows.get(index) {
                    Some(GitRow::Line { path, line }) => Some((path.clone(), line.clone())),
                    _ => None,
                }) else {
                    return div().into_any_element();
                };
                // 删除行不在新文件里，挂批注容易对不上；只在新增 / 上下文行上给 `+`
                let can_comment = line.tag != '-';
                let token = this.session().token;
                let has_comment = this
                    .comments
                    .of(token)
                    .iter()
                    .any(|comment| comment.path == path && comment.line == line.no);
                let target = CommentTarget {
                    token,
                    path: path.clone(),
                    line: line.no,
                    tag: line.tag,
                    snippet: line.text.clone(),
                };
                div()
                    .group("diff-line")
                    .relative()
                    .w_full()
                    .child(diff_line(&line))
                    .when(has_comment, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left_1()
                                .top_1()
                                .size(px(4.))
                                .rounded_full()
                                .bg(rgb(CORAL)),
                        )
                    })
                    .when(can_comment, |d| {
                        let id = format!("diff-comment-{path}-{}", line.no);
                        d.child(
                            div()
                                .id(ElementId::Name(id.into()))
                                .absolute()
                                .right_1()
                                .top_0()
                                .px_1()
                                .rounded_sm()
                                .opacity(0.)
                                .cursor_pointer()
                                .group_hover("diff-line", |d| d.opacity(1.))
                                .child(Icon::new(IconAll::Plus).size_3().text_color(rgb(INK_SOFT)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.begin_comment(target.clone(), window, cx)
                                })),
                        )
                    })
                    .into_any_element()
            }),
        )
        .with_estimated_item_height(row_height)
        // 外层（DiffPane）是块级容器，不认 `flex_1`：直接铺满
        .size_full()
        .into_any_element()
    }

    /// 在后台线程拉一次 git 改动（同一份请求不重复发）。
    pub(crate) fn request_git_diff(&mut self, cx: &mut Context<Self>) {
        let scope = self.right_panel.scope;
        if !scope.git() {
            return;
        }
        let key = {
            let session = &self.sessions[self.active];
            GitDiff::key_of(session, scope)
        };
        let git = &self.right_panel.git;
        if git.loaded == Some(key) || git.pending == Some(key) {
            return;
        }
        self.right_panel.git.pending = Some(key);
        let session = &self.sessions[self.active];
        let request = GitDiffRequest {
            scope,
            dir: session.cwd.clone(),
            // 只取 worktree 元信息里现成的 base（读内存）；普通仓库的基线交给后台线程算
            worktree_base: session.worktree.as_ref().map(|w| w.base.clone()),
        };
        cx.spawn(async move |this, cx| {
            let (rows, error) =
                cx.background_executor().spawn(async move { compute_git_diff(&request) }).await;
            let _ = this.update(cx, |app, cx| {
                // 期间用户换了会话 / 范围：`install` 自己会把迟到的结果丢掉
                app.right_panel.git.install(key, rows, error);
                cx.notify();
            });
        })
        .detach();
    }

    /// 手动重拉（用户手改文件、shell 里改了东西时 `diff_revision` 不会变）。
    pub(crate) fn refresh_git_diff(&mut self, cx: &mut Context<Self>) {
        self.right_panel.git.invalidate();
        self.request_git_diff(cx);
        cx.notify();
    }

    /// 本轮改动文件数：按 `(会话令牌, diff_revision)` 缓存。
    ///
    /// 只按 `diff_revision` 判脏不够：切到另一个恰好同 revision（都是 0 或 2）的会话，
    /// 会沿用上一个会话的 badge 数字。以前每个 Tab 栏重画都全量扫一遍记录
    /// （千文件实测中位 59ms），所以这份缓存也不能省。
    pub(crate) fn refresh_changed_file_count(&mut self) {
        let session = &self.sessions[self.active];
        let key = (session.token, session.diff_revision);
        if self.right_panel.changed_count_key == Some(key) {
            return;
        }
        let count = crate::model::turn_files(&session.transcript, 0).len();
        self.right_panel.changed_count = count;
        self.right_panel.changed_count_key = Some(key);
    }

    /// 文件 Pane 切到后台：只放它自己的预览列表。
    pub(crate) fn release_file_list(&mut self) {
        if let Some(preview) = &mut self.right_panel.preview {
            preview.release_list();
        }
    }

    /// 改动 Pane 切到后台：只放它自己的 Diff 预览列表。
    pub(crate) fn release_diff_list(&mut self) {
        if let Some(preview) = &mut self.right_panel.diff_preview {
            preview.release_list();
        }
        self.right_panel.git.release_list();
    }

    /// 换改动 Pane 的取数范围（顶部那排分段按钮）。
    pub(crate) fn set_diff_scope(&mut self, scope: DiffScope, cx: &mut Context<Self>) {
        if self.right_panel.scope == scope {
            return;
        }
        self.right_panel.scope = scope;
        // git 两档的列表要重新建（行数完全不同）
        self.right_panel.git.release_list();
        // 换档后「选中的那份 diff」可能对不上当前范围，清掉免得两栏打架
        if scope.git() {
            self.right_panel.diff_preview = None;
        }
        cx.notify();
    }

    fn panel_split(&self) -> bool {
        self.right_panel.split
    }

    fn render_file_workspace(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if self.panel_split() {
            let tree = self.render_file_tree(cx);
            let preview = self.render_file_preview(window, cx);
            div()
                .h_flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .w(FILE_TREE_W)
                        .h_full()
                        .flex_shrink_0()
                        .border_r_1()
                        .border_color(rgb(LINE))
                        .child(tree),
                )
                .child(div().flex_1().min_w_0().h_full().child(preview))
                .into_any_element()
        } else if self.right_panel.selected_file.is_some() {
            self.render_file_preview(window, cx)
        } else {
            self.render_file_tree(cx).into_any_element()
        }
    }

    fn render_file_tree(&self, cx: &mut Context<Self>) -> Div {
        let search = div().p_2().border_b_1().border_color(rgb(LINE)).child(
            div()
                .rounded_lg()
                .border_1()
                .border_color(rgb(COMPOSER_LINE))
                .bg(rgb(CARD))
                .px_1()
                .child(
                    Input::new(&self.right_panel.filter_input)
                        .appearance(false)
                        .bordered(false)
                        .small(),
                ),
        );
        let panel = &self.right_panel;
        let count = panel.file_tree.rows.len();
        let body = div().v_flex().flex_1().min_h_0().p_1();
        let body = if count == 0 {
            body.when(panel.file_tree.is_empty_dir(&panel.root), |d| {
                d.child(panel_note("这个目录是空的"))
            })
        } else {
            let app = cx.entity();
            body.child(
                uniform_list("right-file-tree", count, move |range, _, cx| {
                    app.update(cx, |app, cx| {
                        app.right_panel.file_tree.rows[range]
                            .iter()
                            .map(|&row| app.render_file_row(row, cx))
                            .collect()
                    })
                })
                .track_scroll(&panel.file_scroll)
                .flex_1()
                .min_h_0()
                .w_full(),
            )
        };
        div().v_flex().h_full().min_h_0().child(search.flex_shrink_0()).child(body)
    }

    fn render_file_row(&self, row: FileRow, cx: &mut Context<Self>) -> Stateful<Div> {
        let panel = &self.right_panel;
        let node = panel.file_tree.node(row);
        let expanded = node.directory && panel.file_tree.expanded.contains(&node.path);
        let selected = panel.selected_file.as_ref() == Some(&node.path);
        let path = node.path.clone();
        let directory = node.directory;
        div()
            .id(ElementId::Name(format!("right-node-{}", node.path.display()).into()))
            .h_flex()
            .items_center()
            .gap_1()
            .h(FILE_ROW_H)
            .pr_2()
            .pl(rems(0.5 + row.depth as f32 * 0.875))
            .rounded_md()
            .cursor_pointer()
            .text_size(TEXT_SMALL)
            .line_height(LEADING_UI)
            .bg(if selected { rgb(SELECTED) } else { rgba(CLEAR) })
            .when(!selected, |d| d.hover(|d| d.bg(rgb(HOVER))))
            .child(
                Icon::new(if directory {
                    if expanded { IconAll::ChevronDown } else { IconAll::ChevronRight }
                } else {
                    IconAll::FileText
                })
                .size_3p5()
                .flex_shrink_0()
                .text_color(rgb(INK_SOFT)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(rgb(INK))
                    .child(node.name.clone()),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.right_panel.toggle_entry(path.clone(), directory, window.rem_size());
                cx.notify();
            }))
    }

    fn render_file_preview(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let split = self.panel_split();
        let Some(preview) = &mut self.right_panel.preview else {
            return panel_note("选择一个文件查看内容").into_any_element();
        };
        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .h(CHROME_H)
            .flex_shrink_0()
            .px_3()
            .border_b_1()
            .border_color(rgb(LINE))
            .when(!split, |d| {
                d.child(
                    Self::quiet_button("file-preview-back", IconAll::ArrowLeft, Surface::Paper, cx)
                        .small()
                        .tooltip("返回文件树")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.right_panel.selected_file = None;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(preview.path.display().to_string()),
            );
        let body = if let Some(error) = &preview.error {
            panel_note(error).into_any_element()
        } else {
            let rem = window.rem_size();
            let font_size = TEXT_SMALL.to_pixels(rem);
            let row_height = LEADING.to_pixels(font_size.into(), rem);
            let digits = preview.line_starts.len().checked_ilog10().unwrap_or(0) + 1;
            let gutter = px(44.).max(font_size * (0.6 * digits as f32) + rems(0.5).to_pixels(rem));
            list(
                preview.list(rem),
                cx.processor(move |this, index, _, _| {
                    let Some(preview) = &this.right_panel.preview else {
                        return div().into_any_element();
                    };
                    if let Some(line) = preview.line(index) {
                        preview_row(index, line, gutter)
                            .when(index == 0, |row| row.pt_2())
                            .when(
                                !preview.truncated && index + 1 == preview.line_starts.len(),
                                |row| row.pb_2(),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .w_full()
                            .pb_2()
                            .child(panel_note("只显示前 256 KiB"))
                            .into_any_element()
                    }
                }),
            )
            .with_estimated_item_height(row_height)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        div().v_flex().h_full().min_h_0().child(header).child(body).into_any_element()
    }

    fn render_diff_workspace(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let session = &self.sessions[self.active];
        let panel = &mut self.right_panel;
        if panel
            .diff_preview
            .as_ref()
            .is_some_and(|preview| preview.selection.resolve(session).is_none())
        {
            panel.diff_preview = None;
        }
        let split = self.panel_split();
        if split {
            let list = self.render_diff_list(cx);
            let preview = self.render_diff_preview(window, cx);
            div()
                .h_flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .v_flex()
                        .w(FILE_TREE_W)
                        .h_full()
                        .min_h_0()
                        .flex_shrink_0()
                        .border_r_1()
                        .border_color(rgb(LINE))
                        .child(list),
                )
                .child(div().flex_1().min_w_0().h_full().child(preview))
                .into_any_element()
        } else if self.right_panel.diff_preview.is_some() {
            self.render_diff_preview(window, cx).into_any_element()
        } else {
            self.render_diff_list(cx)
        }
    }

    fn render_diff_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let session = &self.sessions[self.active];
        let panel = &mut self.right_panel;
        let offset = panel.scope.start(session);
        ChangedFiles::sync(&mut panel.changed_files, session, offset);
        let count = panel.changed_files.as_ref().map_or(0, |files| files.rows.len());
        if count == 0 {
            return panel_note(match panel.scope {
                DiffScope::Turn => "这一轮还没有文件改动",
                _ => "这个会话还没有文件改动",
            })
            .into_any_element();
        }
        let app = cx.entity();
        div()
            .v_flex()
            .flex_1()
            .min_h_0()
            .py_1()
            .child(
                uniform_list("right-diff-list", count, move |range, _, cx| {
                    app.update(cx, |app, cx| {
                        let session = app.session();
                        let panel = &app.right_panel;
                        let selected = panel
                            .diff_preview
                            .as_ref()
                            .and_then(|preview| preview.selection.resolve(session));
                        let files = panel.changed_files.as_ref().unwrap();
                        files.rows[range]
                            .iter()
                            .map(|file| {
                                let selected =
                                    selected.is_some_and(|diff| same_path(&diff.path, &file.path));
                                app.render_diff_file(file, selected, cx)
                            })
                            .collect()
                    })
                })
                .track_scroll(&panel.diff_list_scroll)
                .flex_1()
                .min_h_0()
                .w_full(),
            )
            .into_any_element()
    }

    fn render_diff_file(
        &self,
        file: &TurnFile,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let session = self.session();
        let origin = (session.token, session.diff_epoch);
        let location = file.location;
        let path = claudesktop_core::review::display_path(&file.path, Some(&session.cwd));
        div()
            .id(ElementId::Name(format!("right-diff-file-{}", file.path).into()))
            .h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .h(rems(2.))
            .px_3()
            .cursor_pointer()
            .bg(if selected { rgb(SELECTED) } else { rgba(CLEAR) })
            .when(!selected, |d| d.hover(|d| d.bg(rgb(HOVER))))
            .child(
                Icon::new(if file.created { IconAll::FilePlus } else { IconAll::FileText })
                    .size_3p5()
                    .text_color(rgb(INK_SOFT)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK))
                    .child(path),
            )
            .child(diff_stat(file.added, file.removed))
            .on_click(cx.listener(move |this, _, window, cx| {
                let session = this.session();
                if (session.token, session.diff_epoch) == origin {
                    this.open_right_diff(location, window, cx);
                }
            }))
    }

    fn render_diff_preview(&mut self, window: &Window, cx: &mut Context<Self>) -> Div {
        let split = self.panel_split();
        let session = &self.sessions[self.active];
        let Some(preview) = &mut self.right_panel.diff_preview else {
            return panel_note("选择一个文件查看 Diff");
        };
        let selection = preview.selection;
        let Some(diff) = selection.resolve(session) else {
            return panel_note("选择一个文件查看 Diff");
        };
        let count = diff.lines.len() + usize::from(diff.truncated);
        let body = if count == 0 {
            preview.release_list();
            preview.scroll_top = ListOffset::default();
            div().py_1().into_any_element()
        } else {
            let rem = window.rem_size();
            let row_height = LEADING.to_pixels(TEXT_SMALL.to_pixels(rem).into(), rem);
            let state = preview.list(count, session.diff_revision, rem);
            let app = cx.entity();
            let token = session.token;
            list(state, move |index, _, cx| {
                let session = app.read(cx).session();
                if session.token == token
                    && let Some(diff) = selection.resolve(session)
                {
                    if let Some(line) = diff.lines.get(index) {
                        return diff_line(line).w_full().into_any_element();
                    }
                    if diff.truncated && index == diff.lines.len() {
                        return diff_truncated().w_full().into_any_element();
                    }
                }
                div().into_any_element()
            })
            .with_estimated_item_height(row_height)
            .flex_1()
            .min_h_0()
            .py_1()
            .into_any_element()
        };
        let card = Self::diff_card(diff, &session.cwd, None, body)
            .when(count > 0, |card| card.flex_1().min_h_0());
        let path = diff.path.clone();
        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_3()
            .h(CHROME_H)
            .flex_shrink_0()
            .border_b_1()
            .border_color(rgb(LINE))
            .when(!split, |d| {
                d.child(
                    Self::quiet_button("diff-preview-back", IconAll::ArrowLeft, Surface::Paper, cx)
                        .small()
                        .tooltip("返回文件列表")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.right_panel.diff_preview = None;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(path),
            );
        div()
            .v_flex()
            .h_full()
            .min_h_0()
            .child(header)
            .child(div().id("right-diff-preview").v_flex().flex_1().min_h_0().p_3().child(card))
    }
    #[cfg(feature = "headless-screenshot")]
    pub(crate) fn debug_right_panel(
        &mut self,
        preview: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (token, cwd) = (self.session().token, self.session().cwd.clone());
        self.right_panel.sync_session(token, &cwd);
        match preview {
            "changes" => {
                use crate::demo::MockAction;
                let session = &mut self.sessions[self.active];
                let item = session.transcript.len();
                session.apply_mock(&MockAction::BeginTool("Edit", "src/main.rs"));
                session.apply_mock(&MockAction::Diff(
                    "src/main.rs",
                    "fn main() {\n    println!(\"old\");\n}\n",
                    "fn main() {\n    println!(\"new\");\n    println!(\"ready\");\n}\n",
                ));
                session.apply_mock(&MockAction::ToolDone("已更新"));
                self.open_right_diff(DiffLocation { item, diff: 0 }, window, cx);
            }
            "expanded" => {
                let path = self.session().cwd.join("apps/desktop/src/main.rs");
                self.right_panel.open = true;
                self.right_panel.tab = RightTab::Files;
                self.right_panel.open_file(path);
                cx.notify();
            }
            _ => {
                self.right_panel.open = true;
                self.right_panel.tab = RightTab::Files;
                if preview == "file-preview" {
                    let path = self.session().cwd.join("apps/desktop/src/main.rs");
                    self.right_panel.open_file(path);
                }
                cx.notify();
            }
        }
    }
}

fn preview_row(index: usize, line: &str, gutter: Pixels) -> Div {
    div()
        .w_full()
        .h_flex()
        .items_start()
        .text_size(TEXT_SMALL)
        .font_family(MONO)
        .line_height(LEADING)
        .child(
            div()
                .w(gutter)
                .flex_shrink_0()
                .pr_2()
                .text_right()
                .whitespace_nowrap()
                .text_color(rgb(INK_FAINT))
                .child((index + 1).to_string()),
        )
        .child(div().flex_1().min_w_0().pr_3().text_color(rgb(INK)).child(if line.is_empty() {
            SharedString::from(" ")
        } else {
            line.to_string().into()
        }))
}

fn panel_note(text: impl Into<SharedString>) -> Div {
    div()
        .p_4()
        .text_size(TEXT_SMALL)
        .text_color(rgb(INK_SOFT))
        .font_weight(WEIGHT_CAPTION)
        .line_height(LEADING)
        .child(text.into())
}

/// 行内批注：选中一行 → 底栏写 → 发给模型。
///
/// 底栏固定挂在面板底部，**不嵌进虚拟列表的行里**：列表是变高虚拟滚动的，行滚出视口
/// 就整个卸载，`InputState` 会丢焦点且每一帧都在整表重测（8ms 滚动红线）。
impl ChatApp {
    /// 取下一条消息要注入的批注说明（会话对不上就丢掉：那是给别的会话留的）。
    pub(crate) fn take_comment_note(&mut self, idx: usize) -> Option<String> {
        let token = self.sessions.get(idx)?.token;
        match self.diff_comment_note.take() {
            Some((owner, note)) if owner == token => Some(note),
            _ => None,
        }
    }

    /// 行内批注的输入框：用到时才建（跟着窗口走，弹出窗里会重建，见 `on_window_changed`）。
    pub(crate) fn ensure_comment_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = &self.comments.input {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("对这几行说点什么…"));
        // 写完直接回车就收下（和底栏的 ✓ 等价）
        cx.subscribe_in(&input, window, |this: &mut ChatApp, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { secondary, .. } if !secondary) {
                this.add_comment(window, cx);
            }
        })
        .detach();
        self.comments.input = Some(input.clone());
        input
    }

    /// 点了 diff 行上的 `+`：记下目标并把焦点送进底栏输入框。
    pub(crate) fn begin_comment(
        &mut self,
        target: CommentTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.ensure_comment_input(window, cx);
        self.comments.target = Some(target);
        input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// 取消这一条（目标和草稿都清掉）。
    pub(crate) fn cancel_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.comments.target = None;
        if let Some(input) = &self.comments.input {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    /// 底栏「添加」：把输入框里的字存进**点这一行时**的那个会话（切会话后目标作废）。
    pub(crate) fn add_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.comments.target.clone() else { return };
        let token = self.session().token;
        if target.token != token {
            // 目标属于别的会话：丢掉，别把 A 的批注写进 B
            self.comments.target = None;
            cx.notify();
            return;
        }
        let Some(input) = self.comments.input.clone() else { return };
        let text = input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.comments.push(
            token,
            DiffComment {
                path: target.path,
                line: target.line,
                tag: target.tag,
                snippet: target.snippet,
                text,
            },
        );
        self.comments.target = None;
        input.update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    /// 底栏「发送」：把批注拼成 `<user-review>` 块，**用发送时注入**的方式带走。
    ///
    /// 直接 `submit_to` 一句可读摘要：说明挂在 `diff_comment_note` 上，由 `submit_to`
    /// 拼进发给内核的文本。这样既不会把原始 XML 露在气泡里，也**不碰主输入框的草稿**，
    /// 还不依赖调用方手里的 `window`（从弹出窗点发送时那是弹出窗）。
    pub(crate) fn send_comments(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let token = self.session().token;
        let comments = self.comments.of(token).to_vec();
        let Some(note) = comments_note(&comments) else { return };
        if self.session().busy {
            self.toast(Notification::warning("正在生成回答：等这一轮结束再发批注"), cx);
            return;
        }
        self.diff_comment_note = Some((token, note));
        let summary = format!("（{} 条行内批注）", comments.len());
        let idx = self.active;
        let sent = self.submit_to(
            idx,
            UserPrompt { text: summary, images: Vec::new(), attachments: Vec::new() },
            cx,
        );
        if !sent {
            // 没发出去：说明不能留着（会被下一条手写消息带走），批注本身留着
            self.diff_comment_note = None;
            return;
        }
        self.comments.clear(token);
        cx.notify();
    }

    /// 丢掉这一会话的批注草稿。
    pub(crate) fn clear_comments(&mut self, cx: &mut Context<Self>) {
        let token = self.session().token;
        self.comments.clear(token);
        self.comments.target = None;
        cx.notify();
    }

    /// 换窗口之后重建输入框，**保留还没提交的草稿**（绑定旧 `Window` 的 InputState 不能再用）。
    pub(crate) fn rebuild_comment_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self
            .comments
            .input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        self.comments.input = None;
        let input = self.ensure_comment_input(window, cx);
        if !draft.is_empty() {
            input.update(cx, |state, cx| state.set_value(draft, window, cx));
        }
    }

    /// 改动 Pane 的底栏：正在写的那一行 + 已有批注数 + 发送。
    fn render_comment_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let token = self.session().token;
        let count = self.comments.of(token).len();
        // 目标属于别的会话（点了 `+` 又切走）：当作没有目标，别把批注写进这个会话
        let target = self.comments.target.clone().filter(|target| target.token == token);
        if target.is_none() && count == 0 {
            return None;
        }
        let input = target.as_ref().map(|_| self.ensure_comment_input(window, cx));
        let mut bar = div()
            .v_flex()
            .flex_shrink_0()
            .w_full()
            .gap_1()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(rgb(LINE))
            .text_size(TEXT_SMALL);
        if let Some(target) = &target {
            bar = bar.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .font_family(MONO)
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .child(target.label())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(target.snippet.trim().to_string()),
                    ),
            );
            if let Some(input) = input {
                bar = bar.child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(Input::new(&input).small()))
                        .child(
                            Self::quiet_button("comment-add", IconAll::Check, Surface::Paper, cx)
                                .small()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.add_comment(window, cx)),
                                ),
                        )
                        .child(
                            Self::quiet_button("comment-cancel", IconAll::X, Surface::Paper, cx)
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.cancel_comment(window, cx)
                                })),
                        ),
                );
            }
        }
        if count > 0 {
            bar = bar.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(rgb(INK_SOFT))
                            .child(format!("{count} 条批注 · 发给模型")),
                    )
                    .child(
                        Self::quiet_button("comments-clear", IconAll::X, Surface::Paper, cx)
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| this.clear_comments(cx))),
                    )
                    .child(
                        Self::quiet_button("comments-send", IconAll::ArrowUp, Surface::Paper, cx)
                            .small()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.send_comments(window, cx)),
                            ),
                    ),
            );
        }
        Some(bar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::KernelChoice;
    use claudesktop_core::KernelEvent;

    #[core::prelude::v1::test]
    fn changed_files_track_completed_diffs_and_reject_truncated_locations() {
        let mut session = Session::new(1, KernelChoice::Builtin, PathBuf::from("project"));
        let mut cache = None;
        let start = |session: &mut Session, id: &str| {
            session.apply_event(
                KernelEvent::ToolCallStarted {
                    id: id.into(),
                    title: "Edit".into(),
                    status: "in_progress".into(),
                    kind: "edit".into(),
                    parent: None,
                },
                None,
            );
        };
        let update = |session: &mut Session, id: &str, status: &str| {
            session.apply_event(
                KernelEvent::ToolCallUpdated {
                    id: id.into(),
                    title: None,
                    status: Some(status.into()),
                    raw_output: None,
                    output: None,
                },
                None,
            );
        };
        let edit = |session: &mut Session, id: &str, path: &str, text: &str| {
            session.apply_event(
                KernelEvent::ToolCallDiff {
                    id: id.into(),
                    diff: DiffView::compute(path, Some("old\n"), text),
                },
                None,
            );
        };
        ChangedFiles::sync(&mut cache, &session, 0);
        start(&mut session, "first");
        edit(&mut session, "first", "src/a.rs", "alpha\n");
        edit(&mut session, "first", "src/b.rs", "beta\n");
        ChangedFiles::sync(&mut cache, &session, 0);
        assert!(cache.as_ref().unwrap().rows.is_empty());
        update(&mut session, "first", "completed");
        ChangedFiles::sync(&mut cache, &session, 0);
        let rows = &cache.as_ref().unwrap().rows;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].location.item, rows[1].location.item);
        let earlier = DiffSelection { location: rows[1].location, epoch: session.diff_epoch };
        assert_eq!(earlier.resolve(&session).unwrap().path, "src/b.rs");

        start(&mut session, "later");
        update(&mut session, "later", "completed");
        edit(&mut session, "later", "src\\B.rs", "latest\n");
        ChangedFiles::sync(&mut cache, &session, 0);
        let rows = &cache.as_ref().unwrap().rows;
        assert_eq!(rows.len(), 2);
        assert_eq!((&*rows[1].path, rows[1].added, rows[1].removed), ("src/b.rs", 2, 2));
        let latest = DiffSelection { location: rows[1].location, epoch: session.diff_epoch };
        assert_eq!(latest.resolve(&session).unwrap().lines.last().unwrap().text, "latest");
        assert_eq!(earlier.resolve(&session).unwrap().lines.last().unwrap().text, "beta");
        update(&mut session, "later", "failed");
        ChangedFiles::sync(&mut cache, &session, 0);
        assert_eq!(cache.as_ref().unwrap().rows[1].location, earlier.location);
        assert!(latest.resolve(&session).is_none());

        session.truncate_to(latest.location.item);
        start(&mut session, "replacement");
        update(&mut session, "replacement", "completed");
        edit(&mut session, "replacement", "replacement.rs", "replacement\n");
        ChangedFiles::sync(&mut cache, &session, 0);
        let rows = &cache.as_ref().unwrap().rows;
        assert_eq!(rows[2].location, latest.location);
        assert_eq!(rows[2].path, "replacement.rs");
        assert!(latest.resolve(&session).is_none());
    }

    #[core::prelude::v1::test]
    fn diff_preview_preserves_scroll_across_list_invalidation() {
        let selection = DiffSelection { location: DiffLocation { item: 1, diff: 0 }, epoch: 0 };
        let mut preview = DiffPreview::new(selection);
        let list = preview.list(400, 1, px(16.));
        list.scroll_to(ListOffset { item_ix: 230, offset_in_item: px(60.) });

        let refreshed = preview.list(400, 2, px(16.));
        assert_eq!(refreshed.logical_scroll_top().item_ix, 230);
        assert_eq!(refreshed.logical_scroll_top().offset_in_item, px(60.));
        preview.release_list();
        assert!(preview.list.is_none());

        let reopened = preview.list(400, 2, px(16.));
        assert_eq!(reopened.logical_scroll_top().item_ix, 230);
        assert_eq!(reopened.logical_scroll_top().offset_in_item, px(60.));
        let shortened = preview.list(16, 3, px(16.));
        assert_eq!(shortened.item_count(), 16);
        assert_eq!(shortened.logical_scroll_top().item_ix, 16);
        assert_eq!(shortened.logical_scroll_top().offset_in_item, px(0.));
    }

    /// 正文里本来就有 `---` / `++` 的行不能被当成文件头吞掉
    /// （Markdown 分割线、SQL 注释、`--flag`、C++ 的 `++i` 都会变成 `---...` / `+++...`）。
    #[core::prelude::v1::test]
    fn diff_body_lines_starting_with_dashes_or_pluses_survive() {
        let text = "diff --git a/doc.md b/doc.md\n\
                    --- a/doc.md\n\
                    +++ b/doc.md\n\
                    @@ -1,2 +1,2 @@\n\
                    --- old separator\n\
                    +--- new separator\n\
                    -- comment\n\
                    ++counter\n";
        let rows = parse_unified_diff(text);
        let body: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                GitRow::Line { line, .. } if line.tag != '~' => Some(line.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            body.contains(&"-- old separator"),
            "hunk 里以 `---` 开头的删除行被吞掉了：{body:?}"
        );
        assert!(body.contains(&"--- new separator"), "hunk 里以 `---` 开头的新增行被吞掉了");
        assert!(body.contains(&"- comment"), "以 `--` 开头的上下文行被吞掉了");
        assert!(body.contains(&"+counter"), "以 `++` 开头的新增行被吞掉了");
    }

    /// 批注拼成 `<user-review>` 块：带上路径、行号、增删标记和那一行的内容；
    /// 空批注（只有空格）不进去，免得给模型一堆空条目。
    #[core::prelude::v1::test]
    fn comments_note_lists_only_written_comments() {
        let comments = vec![
            DiffComment {
                path: "src/a.rs".into(),
                line: 42,
                tag: '+',
                snippet: "let x = 1;".into(),
                text: "这里改成 unwrap_or_default".into(),
            },
            DiffComment {
                path: "src/b.rs".into(),
                line: 7,
                tag: ' ',
                snippet: "}".into(),
                text: "   ".into(),
            },
        ];
        let note = comments_note(&comments).expect("有批注就该有说明");
        assert!(note.starts_with("<user-review>"), "沿用和逐块撤回同一套标签");
        assert!(note.contains("src/a.rs:42 (added)"), "路径 / 行号 / 增删标记都要带上");
        assert!(note.contains("这里改成 unwrap_or_default"));
        assert!(!note.contains("src/b.rs"), "空批注不该发给模型");
        assert!(comments_note(&[]).is_none(), "没有批注就不注入任何东西");
    }

    /// `git diff` 的解析：文件头要落在它那段正文**前面**，增删行数按整段统计。
    #[core::prelude::v1::test]
    fn unified_diff_puts_the_file_header_before_its_body() {
        let text = "diff --git a/src/a.rs b/src/a.rs\n\
                    index 111..222 100644\n\
                    --- a/src/a.rs\n\
                    +++ b/src/a.rs\n\
                    @@ -1,2 +1,3 @@\n\
                     context\n\
                    -old\n\
                    +new\n\
                    +extra\n\
                    diff --git a/b.rs b/b.rs\n\
                    --- a/b.rs\n\
                    +++ b/b.rs\n\
                    @@ -1 +1 @@\n\
                    -gone\n\
                    +here\n";
        let rows = parse_unified_diff(text);
        let GitRow::File { path, added, removed } = &rows[0] else {
            panic!("第一个文件头应该在正文前面")
        };
        assert_eq!(path, "src/a.rs");
        assert_eq!((*added, *removed), (2, 1));
        // hunk 头画成一条分隔
        assert!(matches!(&rows[1], GitRow::Line { line, .. } if line.tag == '~'));
        // 第二个文件也有自己的头
        assert!(rows.iter().any(|row| matches!(row, GitRow::File { path, .. } if path == "b.rs")));
        // `index` / `---` / `+++` 这些不该变成正文行
        assert!(!rows.iter().any(|row| matches!(row, GitRow::Line { line, .. }
            if line.text.starts_with("index ") || line.text.starts_with("---"))));
    }

    #[core::prelude::v1::test]
    fn file_tree_rows_follow_expansion_and_filter() {
        let root = PathBuf::from("project");
        let branch = root.join("Sources");
        let file = branch.join("Sources.rs");
        let node = |path: &Path, directory| FileNode {
            name: path.file_name().unwrap().to_string_lossy().into_owned().into(),
            path: path.to_path_buf(),
            directory,
        };
        let mut tree = FileTree {
            directory_ids: HashMap::from([(root.clone(), 0), (branch.clone(), 1)]),
            directories: vec![
                vec![node(&branch, true), node(&root.join("alpha.txt"), false)],
                vec![node(&file, false)],
            ],
            ..FileTree::default()
        };
        tree.rebuild(&root);
        assert_eq!(tree.rows.len(), 2);
        assert_eq!(tree.position(&file), None);
        tree.expanded.insert(branch.clone());
        tree.rebuild(&root);
        assert_eq!(tree.position(&file), Some(1));
        assert_eq!(tree.rows[1].depth, 1);
        tree.query = "sources".into();
        tree.rebuild(&root);
        assert_eq!(tree.rows.len(), 2);
        tree.query = "sources.rs".into();
        tree.rebuild(&root);
        assert!(tree.rows.is_empty());
        tree.query.clear();
        tree.rebuild(&root);
        assert_eq!(tree.rows.len(), 3);
        tree.expanded.remove(&branch);
        tree.rebuild(&root);
        assert_eq!(tree.rows.len(), 2);
        assert_eq!(tree.directories.len(), 2);
    }

    #[core::prelude::v1::test]
    fn file_preview_reads_text_and_rejects_binary() {
        let dir = std::env::temp_dir().join(format!("claudesktop-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("sample.rs");
        std::fs::write(&text, "fn main() {}\n").unwrap();
        let preview = read_preview(&text);
        assert_eq!(preview.line(0), Some("fn main() {}"));
        assert_eq!(preview.line(1), None);
        assert!(preview.error.is_none());
        assert!(!preview.truncated);
        let binary = dir.join("sample.bin");
        std::fs::write(&binary, [1, 0, 2]).unwrap();
        assert!(read_preview(&binary).error.is_some());
        assert!(read_preview(&dir.join("missing.txt")).error.is_some());
        let mut bytes = vec![b'x'; PREVIEW_LIMIT - 2];
        bytes.extend_from_slice("🙂\n".as_bytes());
        std::fs::write(&text, bytes).unwrap();
        let preview = read_preview(&text);
        assert!(preview.truncated);
        assert!(preview.line(0).unwrap().ends_with('\u{fffd}'));
        assert_eq!(preview.line(1), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[core::prelude::v1::test]
    fn file_preview_line_index_matches_str_lines() {
        let mut preview = FilePreview::default();
        for text in ["", "\n", "\n\n", "\r", "\r\n", "甲\r\n\r\n🙂\n乙\r", "a\rb\n"] {
            preview.set_text(text.into());
            let lines: Vec<_> =
                (0..preview.line_starts.len()).map(|i| preview.line(i).unwrap()).collect();
            assert!(lines.into_iter().eq(text.lines()), "{text:?}");
        }
    }
}
