//! 用量页「请求明细」里的原文弹窗。请求和返回按页读取，大正文按可见范围排版。

mod document;

use self::document::{Document, Text};
use super::{ChatApp, keys, settings_kit::seg_control_in, widgets::Surface};
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_builtin::gateway::RequestLog;
use claudesktop_builtin::request_bodies::{self, Source};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{
    Editor, EditorState, Escape, Input, InputEvent, InputState, Rope, Search,
};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::io::Write as _;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const RAW_W: Rems = rems(56.);
const RAW_H: Rems = rems(30.);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Request,
    Response,
}

impl Tab {
    fn language(self) -> &'static str {
        match self {
            Self::Request => "json",
            Self::Response => "text",
        }
    }

    fn filename(self, name: &str) -> String {
        match self {
            Self::Request => format!("claudesktop-request-{name}.json"),
            Self::Response => format!("claudesktop-response-{name}.txt"),
        }
    }

    fn load(self, source: &Source) -> Result<(Text, SharedString), String> {
        let (text, note) = match self {
            Self::Request => {
                let request = source.request()?;
                let suffix =
                    if request.formatted { "，原样发出（这里排了版）" } else { "" };
                let note = format!("{} 字节{suffix}", request.text.len());
                (request.text, note.into())
            }
            Self::Response => {
                let response = source.response();
                let note = if response.text.is_empty() {
                    "没有存到返回"
                } else if response.truncated {
                    "返回太长，只存了前 2 MB"
                } else {
                    "流式返回是原样的 SSE"
                };
                (response.text, note.into())
            }
        };
        Ok((Text::prepare(text), note))
    }
}

enum Page {
    Unread,
    Loading { _task: Task<()> },
    Ready(Body),
    Error(SharedString),
}

struct Body {
    note: SharedString,
    empty: bool,
    preview: Preview,
}

enum Preview {
    Editor(Entity<EditorState>),
    List(Box<LargePreview>),
}

struct LargePreview {
    document: Arc<Document>,
    list: Option<ListState>,
    scroll_top: ListOffset,
    search: Entity<InputState>,
    search_open: bool,
    query: String,
    hit: Option<Range<usize>>,
    search_task: Option<Task<()>>,
}

impl LargePreview {
    fn list(&mut self, rem: Pixels) -> ListState {
        self.list
            .get_or_insert_with(|| {
                let list = ListState::new(
                    self.document.len(),
                    ListAlignment::Top,
                    rems(4.).to_pixels(rem),
                );
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

    fn show_hit(&mut self) {
        if let Some(hit) = &self.hit {
            self.scroll_top =
                ListOffset { item_ix: self.document.row_at(hit.start), offset_in_item: px(0.) };
            if let Some(list) = &self.list {
                list.scroll_to(self.scroll_top);
            }
        }
    }
}

enum BodyText {
    Editor(Rope),
    List(Arc<Document>),
}

impl BodyText {
    fn copy(&self) -> ClipboardItem {
        let text = match self {
            Self::Editor(text) => text.to_string(),
            Self::List(doc) => doc.text.clone(),
        };
        ClipboardItem::new_string(text)
    }

    fn write(&self, path: &Path) -> std::io::Result<()> {
        match self {
            Self::Editor(text) => {
                let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
                for chunk in text.chunks() {
                    out.write_all(chunk.as_bytes())?;
                }
                out.flush()
            }
            Self::List(doc) => std::fs::write(path, &doc.text),
        }
    }
}

pub(super) struct RawRequest {
    source: Source,
    name: String,
    tab: Tab,
    pages: [Page; 2],
    focus: FocusHandle,
}

impl RawRequest {
    fn new(source: Source, name: String, cx: &mut Context<Self>) -> Self {
        Self {
            source,
            name,
            tab: Tab::Request,
            pages: [Page::Unread, Page::Unread],
            focus: cx.focus_handle(),
        }
    }

    fn load(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.pages[tab as usize], Page::Unread) {
            return;
        }
        let source = self.source.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let loaded = cx.background_executor().spawn(async move { tab.load(&source) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pages[tab as usize] = match loaded {
                    Ok((text, note)) => {
                        Page::Ready(Self::prepare_body(tab, text, note, window, cx))
                    }
                    Err(err) => Page::Error(err.into()),
                };
                cx.notify();
            });
        });
        self.pages[tab as usize] = Page::Loading { _task: task };
    }

    fn prepare_body(
        tab: Tab,
        text: Text,
        note: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Body {
        match text {
            Text::Small(text) => {
                let empty = text.is_empty();
                let editor = cx.new(|cx| {
                    let mut editor = EditorState::new(window, cx)
                        .language(tab.language())
                        .line_number(true)
                        .soft_wrap(true)
                        .searchable(true);
                    editor.set_value(text, window, cx);
                    editor
                });
                Body { note, empty, preview: Preview::Editor(editor) }
            }
            Text::Large(document) => {
                let search = cx.new(|cx| InputState::new(window, cx).placeholder("查找全文"));
                cx.subscribe_in(&search, window, move |this, _, event: &InputEvent, window, cx| {
                    if this.tab == tab {
                        match event {
                            InputEvent::Change => this.search(None, window, cx),
                            InputEvent::PressEnter { shift, .. } => {
                                this.search(Some(*shift), window, cx)
                            }
                            _ => {}
                        }
                    }
                })
                .detach();
                let preview = LargePreview {
                    document: Arc::new(document),
                    list: None,
                    scroll_top: ListOffset::default(),
                    search,
                    search_open: false,
                    query: String::new(),
                    hit: None,
                    search_task: None,
                };
                Body { note, empty: false, preview: Preview::List(Box::new(preview)) }
            }
        }
    }

    fn large_mut(&mut self, tab: Tab) -> Option<&mut LargePreview> {
        match &mut self.pages[tab as usize] {
            Page::Ready(Body { preview: Preview::List(preview), .. }) => Some(preview),
            _ => None,
        }
    }

    fn select_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        if let Some(preview) = self.large_mut(self.tab) {
            preview.release_list();
        }
        self.tab = tab;
        self.load(tab, window, cx);
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn text(&self, cx: &App) -> Option<BodyText> {
        let Page::Ready(Body { preview, empty: false, .. }) = &self.pages[self.tab as usize] else {
            return None;
        };
        Some(match preview {
            Preview::Editor(editor) => BodyText::Editor(editor.read(cx).text().clone()),
            Preview::List(preview) => BodyText::List(preview.document.clone()),
        })
    }

    fn open_in_app(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.text(cx) else { return };
        let path = std::env::temp_dir().join(self.tab.filename(&self.name));
        cx.spawn_in(window, async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { text.write(&path).map(|()| path) })
                .await;
            let _ = cx.update(|window, cx| match result {
                Ok(path) => cx.open_with_system(&path),
                Err(err) => {
                    window.push_notification(Notification::error(format!("写不了原文：{err}")), cx)
                }
            });
        })
        .detach();
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Page::Ready(Body { preview: Preview::Editor(editor), empty: false, .. }) =
            &self.pages[self.tab as usize]
        {
            editor.update(cx, |editor, cx| {
                editor.focus(window, cx);
                editor.open_search(false, cx);
            });
            return;
        }
        let Some(preview) = self.large_mut(self.tab) else {
            cx.propagate();
            return;
        };
        preview.search_open = true;
        let search = preview.search.clone();
        cx.defer_in(window, move |_, window, cx| {
            search.update(cx, |state, cx| state.focus(window, cx));
        });
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(preview) = self.large_mut(self.tab) {
            preview.search_open = false;
            preview.search_task = None;
            preview.query.clear();
            preview.hit = None;
            preview.search.update(cx, |state, cx| state.set_value("", window, cx));
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    fn search(&mut self, previous: Option<bool>, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab;
        let Some(preview) = self.large_mut(tab) else { return };
        if !preview.search_open {
            return;
        }
        let query = preview.search.read(cx).value().to_string();
        let next = previous.is_some() && preview.query == query;
        let previous = previous.unwrap_or(false);
        let offset = if next {
            preview.hit.as_ref().map_or(0, |hit| if previous { hit.start } else { hit.end })
        } else {
            preview.hit = None;
            0
        };
        preview.search_task = None;
        preview.query = query.clone();
        if query.is_empty() {
            cx.notify();
            return;
        }
        let document = preview.document.clone();
        preview.search_task = Some(cx.spawn_in(window, async move |this, cx| {
            if !next {
                cx.background_executor().timer(Duration::from_millis(100)).await;
            }
            let hit = cx
                .background_executor()
                .spawn(
                    async move { request_bodies::find(&document.text, &query, previous, offset) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(preview) = this.large_mut(tab) {
                    preview.search_task = None;
                    preview.hit = hit;
                    preview.show_hit();
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn render_search(&self, preview: &LargePreview, cx: &mut Context<Self>) -> Div {
        let status = if preview.query.is_empty() {
            String::new()
        } else if preview.search_task.is_some() {
            "查找中…".into()
        } else if let Some(hit) = &preview.hit {
            format!("第 {} 行", preview.document.line_at(hit.start))
        } else {
            "没有匹配".into()
        };
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                div().flex_1().min_w_0().child(Input::new(&preview.search).small().cleanable(true)),
            )
            .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(status))
            .child(
                ChatApp::quiet_button("raw-find-prev", IconAll::ChevronUp, Surface::Paper, cx)
                    .small()
                    .tooltip("上一处")
                    .disabled(preview.hit.is_none())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.search(Some(true), window, cx)),
                    ),
            )
            .child(
                ChatApp::quiet_button("raw-find-next", IconAll::ChevronDown, Surface::Paper, cx)
                    .small()
                    .tooltip("下一处")
                    .disabled(preview.hit.is_none())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.search(Some(false), window, cx)),
                    ),
            )
            .child(
                ChatApp::quiet_button("raw-find-close", IconAll::X, Surface::Paper, cx)
                    .small()
                    .tooltip("关闭查找")
                    .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx))),
            )
    }

    fn render_body(&mut self, window: &Window) -> AnyElement {
        match &mut self.pages[self.tab as usize] {
            Page::Ready(Body { preview: Preview::Editor(editor), .. }) => Editor::new(editor)
                .readonly(true)
                .appearance(false)
                .bordered(false)
                .size_full()
                .into_any_element(),
            Page::Ready(Body { preview: Preview::List(preview), .. }) => {
                let rem = window.rem_size();
                let font_size = TEXT_SMALL.to_pixels(rem);
                let row_height = LEADING.to_pixels(font_size.into(), rem);
                let digits = preview.document.line_count.checked_ilog10().unwrap_or(0) + 1;
                let gutter = rems(2.5)
                    .to_pixels(rem)
                    .max(font_size * (digits as f32 * 0.6) + rems(0.5).to_pixels(rem));
                let list_state = preview.list(rem);
                let document = preview.document.clone();
                let hit = preview.hit.clone();
                let body = list(list_state.clone(), move |index, _, _| {
                    let (number, bytes) = document.row(index);
                    let text = &document.text[bytes.clone()];
                    let marked = hit.as_ref().and_then(|hit| {
                        let start = hit.start.max(bytes.start);
                        let end = hit.end.min(bytes.end);
                        (start < end).then(|| start - bytes.start..end - bytes.start)
                    });
                    let text = StyledText::new(if text.is_empty() {
                        SharedString::from(" ")
                    } else {
                        SharedString::from(text.to_owned())
                    })
                    .with_highlights(marked.map(|range| {
                        (
                            range,
                            HighlightStyle {
                                background_color: Some(rgb(ACCENT_SOFT).into()),
                                ..Default::default()
                            },
                        )
                    }));
                    div()
                        .w_full()
                        .h_flex()
                        .items_start()
                        .font_family(MONO)
                        .text_size(TEXT_SMALL)
                        .line_height(LEADING)
                        .child(
                            div()
                                .w(gutter)
                                .flex_shrink_0()
                                .pr_2()
                                .text_right()
                                .text_color(rgb(INK_FAINT))
                                .child(if number == 0 {
                                    String::new()
                                } else {
                                    number.to_string()
                                }),
                        )
                        .child(div().flex_1().min_w_0().pr_3().text_color(rgb(INK)).child(text))
                        .into_any_element()
                })
                .with_estimated_item_height(row_height)
                .size_full();
                div()
                    .size_full()
                    .relative()
                    .child(body)
                    .child(Scrollbar::vertical(&list_state).mode(ScrollbarMode::Always))
                    .into_any_element()
            }
            Page::Error(_) | Page::Loading { .. } | Page::Unread => div().into_any_element(),
        }
    }
}

impl Render for RawRequest {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (note, empty, find, search) = match &self.pages[self.tab as usize] {
            Page::Ready(body) => {
                let large = match &body.preview {
                    Preview::List(preview) => Some(preview),
                    _ => None,
                };
                (
                    body.note.clone(),
                    body.empty,
                    large.is_some(),
                    large.filter(|p| p.search_open).map(|p| self.render_search(p, cx)),
                )
            }
            Page::Error(err) => (err.clone(), true, false, None),
            Page::Loading { .. } | Page::Unread => ("正在读取原文…".into(), true, false, None),
        };
        let height = RAW_H.to_pixels(window.rem_size()).min(window.viewport_size().height * 0.65);
        let body = self.render_body(window);
        div()
            .v_flex()
            .gap_3()
            .key_context(keys::RAW_REQUEST_CONTEXT)
            .on_action(cx.listener(|this, _: &Search, window, cx| this.open_search(window, cx)))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.large_mut(this.tab).is_some_and(|preview| preview.search_open) {
                    this.close_search(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .flex_wrap()
                    .gap_3()
                    .child(seg_control_in(
                        "raw-tab",
                        [(Tab::Request, "请求"), (Tab::Response, "返回")],
                        self.tab,
                        Self::select_tab,
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child(note),
                    )
                    .when(find, |d| {
                        d.child(
                            ChatApp::quiet_button("raw-find", IconAll::Search, Surface::Paper, cx)
                                .small()
                                .tooltip("查找全文")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.open_search(window, cx)),
                                ),
                        )
                    })
                    .child(
                        ChatApp::text_button("raw-copy", "复制", false)
                            .small()
                            .disabled(empty)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(text) = this.text(cx) {
                                    cx.write_to_clipboard(text.copy());
                                }
                            })),
                    )
                    .child(
                        ChatApp::text_button("raw-open", "用编辑器打开", false)
                            .small()
                            .disabled(empty)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_in_app(window, cx)),
                            ),
                    ),
            )
            .children(search)
            .child(
                div()
                    .id("raw-preview")
                    .track_focus(&self.focus)
                    .h(height)
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .overflow_hidden()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .child(body),
            )
    }
}

impl ChatApp {
    pub(super) fn open_raw_request(
        &mut self,
        log: &RequestLog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let (Some(id), Some(dir)) = (&log.body, claudesktop_builtin::gateway::default_dir()) else {
            return;
        };
        let source = match Source::new(&dir, id) {
            Ok(source) => source,
            Err(err) => {
                self.toast(Notification::error(err), cx);
                return;
            }
        };
        let name = id.rsplit('/').next().unwrap_or("request").to_string();
        let view = cx.new(|cx| RawRequest::new(source, name, cx));
        let mut title = vec!["请求原文".to_string()];
        title.extend(log.model.clone());
        title.push(super::requests_view::time_label(log.at_ms));
        let title = title.join(" · ");
        let content = view.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .w(RAW_W.to_pixels(root_font_size()))
                .title(
                    div()
                        .text_size(TEXT_LEAD)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(INK))
                        .child(title.clone()),
                )
                .child(content.clone())
        });
        view.update(cx, |view, cx| {
            view.load(Tab::Request, window, cx);
            view.focus.focus(window, cx);
        });
    }
}
