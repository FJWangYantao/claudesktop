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
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputEvent, InputState};
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
    rows: Vec<TurnFile>,
}

impl ChangedFiles {
    fn sync(cache: &mut Option<Self>, session: &Session) {
        if cache.as_ref().is_none_or(|files| files.revision != session.diff_revision) {
            *cache = Some(Self {
                revision: session.diff_revision,
                rows: turn_files(&session.transcript, 0),
            });
        }
    }
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
    /// 本轮改动文件数（改动 Pane 的 Tab badge 用，按 `diff_revision` 缓存）
    pub(crate) changed_count: usize,
    changed_count_rev: u64,
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
            changed_count_rev: u64::MAX,
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

    /// 改动 Pane 的正文。
    pub(super) fn render_diff_tab(
        &mut self,
        width_rem: Option<f32>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.right_panel.split = width_rem.is_some_and(|w| w >= 36.);
        let session = &self.sessions[self.active];
        self.right_panel.sync_session(session.token, &session.cwd);
        self.refresh_changed_file_count();
        self.render_diff_workspace(window, cx)
    }

    /// 这个面板被切到后台 / 收起时把虚拟列表缓存放掉：
    /// 后台 Tab 不该继续占着行高缓存（见 `HANDOFF.md` 的内存预算一节）。
    pub(crate) fn release_lists(&mut self) {
        if let Some(preview) = &mut self.right_panel.preview {
            preview.release_list();
        }
        if let Some(preview) = &mut self.right_panel.diff_preview {
            preview.release_list();
        }
    }

    /// 本轮改动文件数：按 `diff_revision` 缓存。
    /// 以前每个 Tab 栏重画都全量扫一遍记录（千文件实测中位 59ms）。
    pub(crate) fn refresh_changed_file_count(&mut self) {
        let session = &self.sessions[self.active];
        if self.right_panel.changed_count_rev == session.diff_revision {
            return;
        }
        let count = crate::model::turn_files(&session.transcript, 0).len();
        self.right_panel.changed_count = count;
        self.right_panel.changed_count_rev = session.diff_revision;
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
        ChangedFiles::sync(&mut panel.changed_files, session);
        let count = panel.changed_files.as_ref().map_or(0, |files| files.rows.len());
        if count == 0 {
            return panel_note("这个会话还没有文件改动").into_any_element();
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
    pub(crate) fn debug_right_panel(&mut self, preview: &str, cx: &mut Context<Self>) {
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
        ChangedFiles::sync(&mut cache, &session);
        start(&mut session, "first");
        edit(&mut session, "first", "src/a.rs", "alpha\n");
        edit(&mut session, "first", "src/b.rs", "beta\n");
        ChangedFiles::sync(&mut cache, &session);
        assert!(cache.as_ref().unwrap().rows.is_empty());
        update(&mut session, "first", "completed");
        ChangedFiles::sync(&mut cache, &session);
        let rows = &cache.as_ref().unwrap().rows;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].location.item, rows[1].location.item);
        let earlier = DiffSelection { location: rows[1].location, epoch: session.diff_epoch };
        assert_eq!(earlier.resolve(&session).unwrap().path, "src/b.rs");

        start(&mut session, "later");
        update(&mut session, "later", "completed");
        edit(&mut session, "later", "src\\B.rs", "latest\n");
        ChangedFiles::sync(&mut cache, &session);
        let rows = &cache.as_ref().unwrap().rows;
        assert_eq!(rows.len(), 2);
        assert_eq!((&*rows[1].path, rows[1].added, rows[1].removed), ("src/b.rs", 2, 2));
        let latest = DiffSelection { location: rows[1].location, epoch: session.diff_epoch };
        assert_eq!(latest.resolve(&session).unwrap().lines.last().unwrap().text, "latest");
        assert_eq!(earlier.resolve(&session).unwrap().lines.last().unwrap().text, "beta");
        update(&mut session, "later", "failed");
        ChangedFiles::sync(&mut cache, &session);
        assert_eq!(cache.as_ref().unwrap().rows[1].location, earlier.location);
        assert!(latest.resolve(&session).is_none());

        session.truncate_to(latest.location.item);
        start(&mut session, "replacement");
        update(&mut session, "replacement", "completed");
        edit(&mut session, "replacement", "replacement.rs", "replacement\n");
        ChangedFiles::sync(&mut cache, &session);
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
