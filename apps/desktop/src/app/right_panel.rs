//! 右侧辅助面板：文件树 / 源码预览与逐文件 Diff。
//!
//! 文件系统只在展开目录、刷新和选择文件时读取，渲染帧不扫盘。面板宽度与左侧栏一样可拖拽，
//! 展开为全宽时隐藏对话列；窄面板一次显示树或预览，宽面板并排显示。

use super::ChatApp;
use super::chrome::{CHROME_H, drag_area, window_controls};
use super::menu::diff_stat;
use super::widgets::Surface;
use crate::model::{ChatItem, TurnFile, same_path, turn_files};
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RightTab {
    #[default]
    Files,
    Changes,
}

impl RightTab {
    fn label(self) -> &'static str {
        match self {
            Self::Files => "文件",
            Self::Changes => "改动",
        }
    }

    fn icon(self) -> IconAll {
        match self {
            Self::Files => IconAll::FolderTree,
            Self::Changes => IconAll::FileDiff,
        }
    }
}

#[derive(Clone)]
struct FileNode {
    name: SharedString,
    path: PathBuf,
    directory: bool,
}

struct FilePreview {
    path: PathBuf,
    text: String,
    truncated: bool,
    error: Option<String>,
}

pub(super) struct RightPanelState {
    pub open: bool,
    pub expanded: bool,
    pub tab: RightTab,
    pub width_delta: f32,
    seen_token: Option<u64>,
    root: PathBuf,
    entries: HashMap<PathBuf, Vec<FileNode>>,
    expanded_dirs: HashSet<PathBuf>,
    selected_file: Option<PathBuf>,
    preview: Option<FilePreview>,
    diff_selected: Option<usize>,
    filter_input: Entity<InputState>,
    query: String,
    file_scroll: ScrollHandle,
    preview_scroll: ScrollHandle,
    diff_list_scroll: ScrollHandle,
    diff_scroll: ScrollHandle,
}

impl RightPanelState {
    pub fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("筛选文件"));
        cx.subscribe_in(
            &filter_input,
            window,
            |this: &mut ChatApp, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = this.right_panel.filter_input.read(cx).value().to_string();
                    this.right_panel.query = query;
                    cx.notify();
                }
            },
        )
        .detach();
        Self {
            open: false,
            expanded: false,
            tab: RightTab::Files,
            width_delta: 0.,
            seen_token: None,
            root: PathBuf::new(),
            entries: HashMap::new(),
            expanded_dirs: HashSet::new(),
            selected_file: None,
            preview: None,
            diff_selected: None,
            filter_input,
            query: String::new(),
            file_scroll: ScrollHandle::new(),
            preview_scroll: ScrollHandle::new(),
            diff_list_scroll: ScrollHandle::new(),
            diff_scroll: ScrollHandle::new(),
        }
    }

    fn width_rem(&self) -> f32 {
        (PANEL_DEFAULT_W.0 + self.width_delta).clamp(20., 48.)
    }

    fn sync_session(&mut self, token: u64, cwd: &Path) {
        if self.seen_token == Some(token) && self.root == cwd {
            return;
        }
        self.seen_token = Some(token);
        self.root = cwd.to_path_buf();
        self.refresh();
    }

    fn refresh(&mut self) {
        self.entries.clear();
        self.expanded_dirs.clear();
        self.selected_file = None;
        self.preview = None;
        self.diff_selected = None;
        self.load_dir(&self.root.clone());
    }

    fn load_dir(&mut self, dir: &Path) {
        if self.entries.contains_key(dir) {
            return;
        }
        let mut nodes: Vec<FileNode> = std::fs::read_dir(dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let kind = entry.file_type().ok()?;
                let path = entry.path();
                Some(FileNode {
                    name: entry.file_name().to_string_lossy().into_owned().into(),
                    directory: kind.is_dir(),
                    path,
                })
            })
            .collect();
        nodes.sort_by_key(|node| (!node.directory, node.name.to_string().to_lowercase()));
        self.entries.insert(dir.to_path_buf(), nodes);
    }

    fn toggle_entry(&mut self, path: PathBuf, directory: bool) {
        if directory {
            self.load_dir(&path);
            if !self.expanded_dirs.remove(&path) {
                self.expanded_dirs.insert(path);
            }
        } else {
            self.open_file(path);
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        let preview = read_preview(&path);
        self.preview_scroll.set_offset(point(px(0.), px(0.)));
        self.selected_file = Some(path);
        self.preview = Some(preview);
    }

    fn matches(&self, node: &FileNode) -> bool {
        let query = self.query.trim().to_lowercase();
        query.is_empty() || node.name.to_string().to_lowercase().contains(&query)
    }
}

fn read_preview(path: &Path) -> FilePreview {
    let mut result = FilePreview {
        path: path.to_path_buf(),
        text: String::new(),
        truncated: false,
        error: None,
    };
    let read = (|| -> std::io::Result<()> {
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take((PREVIEW_LIMIT + 1) as u64).read_to_end(&mut bytes)?;
        result.truncated = bytes.len() > PREVIEW_LIMIT;
        bytes.truncate(PREVIEW_LIMIT);
        if bytes.contains(&0) {
            result.error = Some("这是二进制文件，不能按文本预览".into());
        } else {
            result.text = String::from_utf8_lossy(&bytes).into_owned();
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

    pub(super) fn toggle_right_panel(&mut self, tab: RightTab, cx: &mut Context<Self>) {
        if self.right_panel.open && self.right_panel.tab == tab {
            self.right_panel.open = false;
            self.right_panel.expanded = false;
        } else {
            self.settings_open = false;
            self.right_panel.open = true;
            self.right_panel.tab = tab;
        }
        cx.notify();
    }

    pub(super) fn open_right_diff(&mut self, item: usize, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.right_panel.open = true;
        self.right_panel.tab = RightTab::Changes;
        self.right_panel.diff_selected = Some(item);
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

    fn render_right_handle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("right-panel-resize")
            .w(px(6.))
            .h_full()
            .flex_shrink_0()
            .cursor_ew_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.panel_resizing = Some(PanelResize {
                        start_x: event.position.x,
                        start_width: this.right_panel.width_rem(),
                        right: true,
                    });
                    window.prevent_default();
                    cx.stop_propagation();
                }),
            )
    }

    pub(super) fn render_right_panel_slot(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.right_panel.open || self.settings_open || self.session().is_chat() {
            return None;
        }
        let (token, cwd) = (self.session().token, self.session().cwd.clone());
        self.right_panel.sync_session(token, &cwd);
        let expanded = self.right_panel.expanded;
        let body = match self.right_panel.tab {
            RightTab::Files => self.render_file_workspace(cx),
            RightTab::Changes => self.render_diff_workspace(cx),
        };
        let header = self.render_right_header(window, cx);
        Some(
            div()
                .h_flex()
                .h_full()
                .flex_shrink_0()
                .when(!expanded, |d| d.w(rems(self.right_panel.width_rem())))
                .when(expanded, |d| d.flex_1().min_w_0())
                .child(self.render_right_handle(cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .bg(rgb(PAPER))
                        .border_l_1()
                        .border_color(rgb(LINE))
                        .child(header)
                        .child(body),
                ),
        )
    }

    fn render_right_header(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let tab_button = |tab: RightTab, cx: &mut Context<Self>| {
            let active = self.right_panel.tab == tab;
            div()
                .id(ElementId::Name(format!("right-tab-{}", tab.label()).into()))
                .h_flex()
                .items_center()
                .gap_1p5()
                .h(rems(1.75))
                .px_2p5()
                .rounded_md()
                .cursor_pointer()
                .text_size(TEXT_SMALL)
                .line_height(LEADING_UI)
                .text_color(rgb(if active { INK } else { INK_SOFT }))
                .bg(if active { rgb(SELECTED) } else { rgba(CLEAR) })
                .when(!active, |d| d.hover(|d| d.bg(rgb(HOVER))))
                .child(Icon::new(tab.icon()).size_3p5())
                .child(tab.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.right_panel.tab = tab;
                    cx.notify();
                }))
        };
        let expanded = self.right_panel.expanded;
        div()
            .h_flex()
            .h(CHROME_H)
            .flex_shrink_0()
            .items_center()
            .pl_3()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(tab_button(RightTab::Files, cx))
            .child(tab_button(RightTab::Changes, cx))
            .child(drag_area())
            .child(
                Self::quiet_button("right-refresh", IconAll::RefreshCw, Surface::Paper, cx)
                    .small()
                    .tooltip("刷新")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.right_panel.refresh();
                        cx.notify();
                    })),
            )
            .child(
                Self::quiet_button(
                    "right-expand",
                    if expanded { IconAll::WindowRestore } else { IconAll::WindowMaximize },
                    Surface::Paper,
                    cx,
                )
                .small()
                .tooltip(if expanded { "恢复对话并排" } else { "展开到全宽" })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.right_panel.expanded = !this.right_panel.expanded;
                    cx.notify();
                })),
            )
            .child(
                Self::quiet_button("right-close", IconAll::X, Surface::Paper, cx)
                    .small()
                    .tooltip("关闭辅助面板")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.right_panel.open = false;
                        this.right_panel.expanded = false;
                        cx.notify();
                    })),
            )
            .children(window_controls(window))
    }

    fn panel_split(&self) -> bool {
        self.right_panel.expanded || self.right_panel.width_rem() >= 36.
    }

    fn render_file_workspace(&self, cx: &mut Context<Self>) -> AnyElement {
        let tree = self.render_file_tree(cx);
        let preview = self.render_file_preview(cx);
        if self.panel_split() {
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
            preview
        } else {
            tree.into_any_element()
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
        let mut rows = div().v_flex().p_1();
        let root = self.right_panel.root.clone();
        if let Some(nodes) = self.right_panel.entries.get(&root) {
            rows = rows.children(self.render_tree_nodes(&root, nodes, 0, cx));
        }
        if self.right_panel.entries.get(&root).is_some_and(Vec::is_empty) {
            rows = rows.child(panel_note("这个目录是空的"));
        }
        div().v_flex().h_full().min_h_0().child(search).child(
            div()
                .id("right-file-tree")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.right_panel.file_scroll)
                .child(rows),
        )
    }

    fn render_tree_nodes(
        &self,
        parent: &Path,
        nodes: &[FileNode],
        depth: usize,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let mut out = Vec::new();
        if depth > MAX_DEPTH {
            return out;
        }
        for node in nodes.iter().filter(|node| self.right_panel.matches(node)) {
            let expanded = self.right_panel.expanded_dirs.contains(&node.path);
            let selected = self.right_panel.selected_file.as_ref() == Some(&node.path);
            let path = node.path.clone();
            let directory = node.directory;
            let row = div()
                .id(ElementId::Name(format!("right-node-{}", node.path.display()).into()))
                .h_flex()
                .items_center()
                .gap_1()
                .h(rems(1.75))
                .pr_2()
                .pl(rems(0.5 + depth as f32 * 0.875))
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.right_panel.toggle_entry(path.clone(), directory);
                    cx.notify();
                }));
            out.push(row);
            if directory && expanded {
                let child_path = parent.join(node.name.as_ref());
                if let Some(children) = self.right_panel.entries.get(&child_path) {
                    out.extend(self.render_tree_nodes(&child_path, children, depth + 1, cx));
                }
            }
        }
        out
    }

    fn render_file_preview(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(preview) = &self.right_panel.preview else {
            return panel_note("选择一个文件查看内容").into_any_element();
        };
        let split = self.panel_split();
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
        let body: AnyElement = if let Some(error) = &preview.error {
            panel_note(error).into_any_element()
        } else {
            let mut lines = div().v_flex().py_2();
            for (n, line) in preview.text.lines().enumerate() {
                lines = lines.child(
                    div()
                        .h_flex()
                        .items_start()
                        .text_size(TEXT_SMALL)
                        .font_family(MONO)
                        .line_height(LEADING)
                        .child(
                            div()
                                .w(px(44.))
                                .flex_shrink_0()
                                .pr_2()
                                .text_right()
                                .text_color(rgb(INK_FAINT))
                                .child((n + 1).to_string()),
                        )
                        .child(div().flex_1().min_w_0().pr_3().text_color(rgb(INK)).child(
                            if line.is_empty() { " ".to_string() } else { line.to_string() },
                        )),
                );
            }
            if preview.truncated {
                lines = lines.child(panel_note("只显示前 256 KiB"));
            }
            div()
                .id("right-file-preview")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.right_panel.preview_scroll)
                .child(lines)
                .into_any_element()
        };
        div().v_flex().h_full().min_h_0().child(header).child(body).into_any_element()
    }

    fn render_diff_workspace(&self, cx: &mut Context<Self>) -> AnyElement {
        let list = self.render_diff_list(cx);
        let preview = self.render_diff_preview(cx);
        if self.panel_split() {
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
                        .child(list),
                )
                .child(div().flex_1().min_w_0().h_full().child(preview))
                .into_any_element()
        } else if self.right_panel.diff_selected.is_some() {
            preview.into_any_element()
        } else {
            list
        }
    }

    fn render_diff_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let files = turn_files(&self.session().transcript, 0);
        if files.is_empty() {
            return panel_note("这个会话还没有文件改动").into_any_element();
        }
        let cwd = self.session().cwd.clone();
        let rows = files.into_iter().map(|file| {
            let item = file.item;
            let path = claudesktop_core::review::display_path(&file.path, Some(&cwd));
            let selected = self.right_panel.diff_selected == Some(item);
            div()
                .id(("right-diff-file", item))
                .h_flex()
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.right_panel.diff_selected = Some(item);
                    cx.notify();
                }))
        });
        div()
            .id("right-diff-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.right_panel.diff_list_scroll)
            .child(div().v_flex().py_1().children(rows))
            .into_any_element()
    }

    fn render_diff_preview(&self, cx: &mut Context<Self>) -> Div {
        let Some(item) = self.right_panel.diff_selected else {
            return panel_note("选择一个文件查看 Diff");
        };
        let session = self.session();
        let Some((card, diff)) = find_diff(session, item) else {
            return panel_note("这份 Diff 已不在当前会话里");
        };
        let path = diff.path.clone();
        let cwd = session.cwd.clone();
        let split = self.panel_split();
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
                            this.right_panel.diff_selected = None;
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
        let _ = card;
        div().v_flex().h_full().min_h_0().child(header).child(
            div()
                .id("right-diff-preview")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.right_panel.diff_scroll)
                .child(div().p_3().child(Self::render_diff(diff, &cwd, None, None, None))),
        )
    }
    #[cfg(feature = "headless-screenshot")]
    pub(crate) fn debug_right_panel(&mut self, preview: &str, cx: &mut Context<Self>) {
        let (token, cwd) = (self.session().token, self.session().cwd.clone());
        self.right_panel.sync_session(token, &cwd);
        match preview {
            "changes" => {
                let item = self.session().transcript.len();
                let diff = DiffView::compute(
                    "src/main.rs",
                    Some("fn main() {\n    println!(\"old\");\n}\n"),
                    "fn main() {\n    println!(\"new\");\n    println!(\"ready\");\n}\n",
                );
                self.sessions[self.active].transcript.push(ChatItem::Tool(
                    crate::model::ToolCard {
                        id: None,
                        kind: "edit".into(),
                        name: "Edit".into(),
                        arg: "src/main.rs".into(),
                        result: "已更新".into(),
                        output: SharedString::default(),
                        content: SharedString::default(),
                        phase: crate::model::ToolPhase::Done,
                        diffs: vec![diff],
                        expanded: false,
                        task: false,
                        grant: None,
                        images: Vec::new(),
                    },
                ));
                self.open_right_diff(item, cx);
            }
            "expanded" => {
                let path = self.session().cwd.join("apps/desktop/src/main.rs");
                self.right_panel.open = true;
                self.right_panel.expanded = true;
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

fn find_diff(session: &Session, item: usize) -> Option<(&crate::model::ToolCard, &DiffView)> {
    let ChatItem::Tool(card) = session.transcript.get(item)? else {
        return None;
    };
    let file: TurnFile = turn_files(&session.transcript, 0).into_iter().find(|f| f.item == item)?;
    card.diffs.iter().find(|diff| same_path(&diff.path, &file.path)).map(|diff| (card, diff))
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

    #[core::prelude::v1::test]
    fn file_preview_reads_text_and_rejects_binary() {
        let dir = std::env::temp_dir().join(format!("claudesktop-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("sample.rs");
        std::fs::write(&text, "fn main() {}\n").unwrap();
        let preview = read_preview(&text);
        assert_eq!(preview.text, "fn main() {}\n");
        assert!(preview.error.is_none());
        assert!(!preview.truncated);
        let binary = dir.join("sample.bin");
        std::fs::write(&binary, [1, 0, 2]).unwrap();
        assert!(read_preview(&binary).error.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
