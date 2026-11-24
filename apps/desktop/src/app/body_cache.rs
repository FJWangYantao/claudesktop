use super::{ChatApp, chat, math, remote_image};
use crate::session::ListMetrics;
use crate::settings::CodeThemeChoice;
use crate::theme::{INK, INK_SOFT, LEADING, TEXT_BODY, TEXT_SMALL, rgb};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

const CAPACITY: usize = 96;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum BodyKind {
    Agent(Option<usize>),
    Think,
    Compaction,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct CodeCopy {
    pub base: usize,
    pub copied: Option<(usize, Instant)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BodyKey {
    src: usize,
    kind: BodyKind,
}

#[derive(Default)]
pub(crate) struct BodyViewCache {
    entries: RefCell<BodyViews>,
}

#[derive(Default)]
struct BodyViews {
    clock: u64,
    entries: HashMap<BodyKey, (u64, Entity<AgentBodyView>)>,
}

impl BodyViews {
    fn get_or_insert(
        &mut self,
        key: BodyKey,
        make_view: impl FnOnce() -> Entity<AgentBodyView>,
    ) -> Entity<AgentBodyView> {
        self.clock += 1;
        let now = self.clock;
        if let Some((used, view)) = self.entries.get_mut(&key) {
            *used = now;
            return view.clone();
        }
        if self.entries.len() == CAPACITY
            && let Some((&oldest, _)) = self.entries.iter().min_by_key(|(_, (used, _))| *used)
        {
            self.entries.remove(&oldest);
        }
        let view = make_view();
        self.entries.insert(key, (now, view.clone()));
        view
    }
}

impl BodyViewCache {
    pub(crate) fn clear(&mut self) {
        self.entries.get_mut().entries.clear();
    }

    pub(crate) fn truncate(&mut self, src: usize) {
        self.entries.get_mut().entries.retain(|key, _| key.src < src);
    }

    pub(super) fn body(
        &self,
        src: usize,
        kind: BodyKind,
        text: &str,
        streaming: bool,
        copy: Option<CodeCopy>,
        cx: &mut Context<ChatApp>,
    ) -> Entity<AgentBodyView> {
        let key = BodyKey { src, kind };
        let view = self.entries.borrow_mut().get_or_insert(key, || {
            let app = cx.entity().downgrade();
            cx.new(|cx| AgentBodyView::new(key, text, streaming, copy, app, cx))
        });
        let body = view.read(cx);
        let text_changed = body.source.as_ref() != text;
        let images_changed = body.has_images && body.image_revision != remote_image::revision();
        if text_changed || body.streaming != streaming || body.copy != copy || images_changed {
            view.update(cx, |body, cx| {
                if text_changed || body.streaming != streaming {
                    let normalized = normalized_text(kind, text, streaming);
                    body.text_state.update(cx, |state, cx| state.set_text(&normalized, cx));
                    body.source = text.into();
                    body.has_images = has_remote_media(text);
                }
                body.image_revision = remote_image::revision();
                body.streaming = streaming;
                body.copy = copy;
                body.invalidate(cx);
            });
        }
        view
    }
}

fn normalized_text(kind: BodyKind, text: &str, streaming: bool) -> SharedString {
    chat::reading_markdown(if kind == BodyKind::Think { text.trim() } else { text }, streaming)
}

fn has_remote_media(text: &str) -> bool {
    text.contains("http://") || text.contains("https://")
}

#[derive(Clone, Copy, PartialEq)]
struct BodyLayout {
    metrics: ListMetrics,
    scale: f32,
    palette: usize,
    dark: bool,
    code_theme: CodeThemeChoice,
    revision: u64,
}

pub(super) struct AgentBodyView {
    text_state: Entity<TextViewState>,
    key: BodyKey,
    source: Box<str>,
    streaming: bool,
    copy: Option<CodeCopy>,
    app: WeakEntity<ChatApp>,
    has_images: bool,
    image_revision: u64,
    content_rev: u64,
    layout: Cell<Option<BodyLayout>>,
    measured: Rc<Cell<Option<(BodyLayout, Pixels)>>>,
}

impl AgentBodyView {
    fn new(
        key: BodyKey,
        text: &str,
        streaming: bool,
        copy: Option<CodeCopy>,
        app: WeakEntity<ChatApp>,
        cx: &mut Context<Self>,
    ) -> Self {
        let normalized = normalized_text(key.kind, text, streaming);
        let text_state = cx.new(|cx| TextViewState::markdown(&normalized, cx));
        // The keyed-state constructor used to wire this up. Keep async parsing and selection live.
        cx.observe(&text_state, |this, _, cx| this.invalidate(cx)).detach();
        Self {
            text_state,
            key,
            source: text.into(),
            streaming,
            copy,
            app,
            has_images: has_remote_media(text),
            image_revision: remote_image::revision(),
            content_rev: 0,
            layout: Cell::new(None),
            measured: Rc::new(Cell::new(None)),
        }
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.content_rev = self.content_rev.wrapping_add(1);
        self.measured.set(None);
        cx.notify();
    }

    pub(super) fn element(
        view: Entity<Self>,
        metrics: Option<ListMetrics>,
        moving: bool,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let body = view.read(cx);
        let layout = metrics.map(|metrics| BodyLayout {
            metrics,
            scale: window.scale_factor(),
            palette: crate::theme::palette_index(),
            dark: crate::theme::is_dark(),
            code_theme: crate::syntax::code_theme(),
            revision: body.content_rev,
        });
        body.layout.set(layout);
        let height = body.measured.get().and_then(|(measured, height)| {
            (Some(measured) == layout && height > px(0.)).then_some(height)
        });
        if let Some(height) = height
            && !body.streaming
            && body.copy.is_none_or(|copy| copy.copied.is_none())
            && !window.is_a11y_active()
            && window.captured_hitbox().is_none()
            && !cx.has_active_drag()
        {
            let state = body.text_state.read(cx);
            if !state.is_selecting()
                && state.selected_source_range().is_none()
                && !state.focus_handle().is_focused(window)
            {
                return view
                    .cached_scroll(StyleRefinement::default().w_full().h(height), moving)
                    .into_any_element();
            }
        }
        view.into_any_element()
    }
}

impl Render for AgentBodyView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (style, size, color) = match self.key.kind {
            BodyKind::Agent(_) => (chat::markdown_style(), TEXT_BODY, None),
            BodyKind::Think => (chat::think_markdown_style(), TEXT_SMALL, Some(INK_SOFT)),
            BodyKind::Compaction => (chat::markdown_style(), TEXT_BODY, Some(INK)),
        };
        let mut view = TextView::new(&self.text_state)
            .style(style)
            .markdown_extensions(math::extensions())
            .text_size(size)
            .line_height(LEADING)
            .when_some(color, |view, color| view.text_color(rgb(color)));
        if let Some(copy) = self.copy {
            let src = self.key.src;
            let app = self.app.clone();
            view = view.stream_fade(self.streaming).code_block_actions(move |block, window, cx| {
                let start = copy.base + block.span.map_or(0, |span| span.start);
                let code = block.code();
                let app = app.clone();
                chat::code_block_header(
                    block.lang(),
                    copy.copied.filter(|(offset, _)| *offset == start).map(|(_, at)| at),
                    move |_, _, cx| {
                        let _ = app.update(cx, |this, cx| {
                            this.copy_to_clipboard(src, Some(start), code.to_string(), cx)
                        });
                    },
                    window,
                    cx,
                )
            });
        }
        let layout = self.layout.get();
        let measured = self.measured.clone();
        div().w_full().min_w_0().child(view).on_children_prepainted(move |bounds, window, _| {
            if let (Some(layout), Some(child)) = (layout, bounds.first()) {
                let previous = measured.replace(Some((layout, child.size.height)));
                if let Some((_, previous)) = previous
                    && previous != child.size.height
                {
                    window.request_animation_frame();
                }
            }
        })
    }
}

#[cfg(all(test, feature = "headless-screenshot"))]
mod tests {
    use super::{AgentBodyView, BodyKey, BodyKind, BodyViews, CAPACITY};
    use gpui_screenshot::{self as gpui, AppContext as _, WeakEntity};

    #[gpui::test]
    fn body_cache_retains_recent_entities_and_evicts_the_oldest(cx: &mut gpui::TestAppContext) {
        let mut cache = BodyViews::default();
        let key = |src| BodyKey { src, kind: BodyKind::Agent(None) };
        let mut insert = |cache: &mut BodyViews, src| {
            cache.get_or_insert(key(src), || {
                cx.new(|cx| {
                    AgentBodyView::new(key(src), "", false, None, WeakEntity::new_invalid(), cx)
                })
            })
        };
        let first = insert(&mut cache, 0);
        for src in 1..CAPACITY {
            insert(&mut cache, src);
        }
        assert_eq!(insert(&mut cache, 0).entity_id(), first.entity_id());
        insert(&mut cache, CAPACITY);
        assert_eq!(cache.entries.len(), CAPACITY);
        assert!(cache.entries.contains_key(&key(0)));
        assert!(!cache.entries.contains_key(&key(1)));
    }
}
