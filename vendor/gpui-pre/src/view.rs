use crate::scene::RecordedScene;
use crate::{
    AnyElement, AnyEntity, AnyWeakEntity, App, AvailableSpace, Bounds, ContentMask, Context,
    Element, ElementId, Entity, EntityId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, PaintIndex, Pixels, PrepaintStateIndex, Render, RenderOnce, Size, Style,
    StyleRefinement, TextStyle, WeakEntity,
};
use crate::{Empty, Window};
use crate::{Hitbox, Point};
use anyhow::Result;
use collections::FxHashSet;
use refineable::Refineable;
use std::mem;
use std::{
    any::{TypeId, type_name},
    fmt,
    ops::Range,
};

/// A dynamically-typed view handle that can be downcast to a specific `Entity<V>`.
///
/// This is the type-erased counterpart to [`ViewElement`]: it holds an entity plus
/// a function pointer to its render, and is itself a [`View`], so embedding it as an
/// element goes through the same [`ViewElement`] machinery as any other view.
#[derive(Clone, Debug)]
pub struct AnyView {
    entity: AnyEntity,
    render: fn(&AnyView, &mut Window, &mut App) -> AnyElement,
    view_type_name: &'static str,
}

impl<V: Render> From<Entity<V>> for AnyView {
    fn from(value: Entity<V>) -> Self {
        AnyView {
            entity: value.into_any(),
            render: any_view::render::<V>,
            view_type_name: type_name::<V>(),
        }
    }
}

impl AnyView {
    /// Embed this view as a cached [`ViewElement`] laid out at `style`.
    ///
    /// The rendered subtree is recycled from the previous frame unless
    /// [Context::notify] was called on the backing entity since it was rendered
    /// (or [Window::refresh] is called, which ignores caching).
    pub fn cached(self, style: StyleRefinement) -> ViewElement<AnyView> {
        self.into_element().cached(style)
    }

    /// Convert this to a weak handle.
    pub fn downgrade(&self) -> AnyWeakView {
        AnyWeakView {
            entity: self.entity.downgrade(),
            render: self.render,
            view_type_name: self.view_type_name,
        }
    }

    /// Convert this to a [Entity] of a specific type.
    /// If this handle does not contain a view of the specified type, returns itself in an `Err` variant.
    pub fn downcast<T: 'static>(self) -> Result<Entity<T>, Self> {
        match self.entity.downcast() {
            Ok(entity) => Ok(entity),
            Err(entity) => Err(Self {
                entity,
                render: self.render,
                view_type_name: self.view_type_name,
            }),
        }
    }

    /// Gets the [TypeId] of the underlying view.
    pub fn entity_type(&self) -> TypeId {
        self.entity.entity_type
    }

    /// The [`EntityId`] of this view.
    pub fn entity_id(&self) -> EntityId {
        self.entity.entity_id()
    }
}

impl PartialEq for AnyView {
    fn eq(&self, other: &Self) -> bool {
        self.entity == other.entity
    }
}

impl Eq for AnyView {}

/// `AnyView` is the type-erased [`View`]: its `render` is a function pointer rather
/// than a concrete type, but it participates in the reactive graph exactly like any
/// other view via [`ViewElement`].
impl View for AnyView {
    fn entity_id(&self) -> Option<EntityId> {
        Some(self.entity.entity_id())
    }

    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        (self.render)(&self, window, cx)
    }
}

impl<V: 'static + Render> IntoElement for Entity<V> {
    type Element = ViewElement<Entity<V>>;

    fn into_element(self) -> Self::Element {
        ViewElement::with_type_name(self, type_name::<V>())
    }

    #[inline(never)]
    fn into_any_element(self) -> AnyElement {
        self.into_element().into_any()
    }
}

impl IntoElement for AnyView {
    type Element = ViewElement<AnyView>;

    fn into_element(self) -> Self::Element {
        let view_type_name = self.view_type_name;
        ViewElement::with_type_name(self, view_type_name)
    }
}

/// A weak, dynamically-typed view handle.
pub struct AnyWeakView {
    entity: AnyWeakEntity,
    render: fn(&AnyView, &mut Window, &mut App) -> AnyElement,
    view_type_name: &'static str,
}

impl AnyWeakView {
    /// Upgrade to a strong `AnyView` handle, if the view is still alive.
    pub fn upgrade(&self) -> Option<AnyView> {
        let entity = self.entity.upgrade()?;
        Some(AnyView {
            entity,
            render: self.render,
            view_type_name: self.view_type_name,
        })
    }
}

impl<V: 'static + Render> From<WeakEntity<V>> for AnyWeakView {
    fn from(view: WeakEntity<V>) -> Self {
        AnyWeakView {
            entity: view.into(),
            render: any_view::render::<V>,
            view_type_name: type_name::<V>(),
        }
    }
}

impl PartialEq for AnyWeakView {
    fn eq(&self, other: &Self) -> bool {
        self.entity == other.entity
    }
}

impl std::fmt::Debug for AnyWeakView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnyWeakView")
            .field("entity_id", &self.entity.entity_id)
            .finish_non_exhaustive()
    }
}

mod any_view {
    use crate::{AnyElement, AnyView, App, IntoElement, Render, Window};

    pub(crate) fn render<V: 'static + Render>(
        view: &AnyView,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let view = view.clone().downcast::<V>().unwrap();
        // Record the view's Render type name so the accessibility debug dump can
        // attribute nodes to the view that produced them.
        #[cfg(debug_assertions)]
        window
            .a11y
            .view_type_names
            .insert(view.entity_id(), std::any::type_name::<V>());
        view.update(cx, |view, cx| view.render(window, cx).into_any_element())
    }
}

/// A renderable that participates in GPUI's reactive graph — the unifying model
/// behind [`Render`] and [`RenderOnce`].
///
/// When `entity_id()` returns `Some`, that id becomes the view's identity: it gets
/// a unique element-id space (so internal `use_state` / `.id(..)` never collide
/// across siblings) and `cx.notify()` on that entity re-renders only this view's
/// subtree. `None` behaves like a stateless component.
///
/// You rarely implement `View` directly. `Entity<T: Render>` and any `T: RenderOnce`
/// get a blanket impl below; implement it by hand only when a component needs both
/// parent-supplied props *and* a backing entity for identity.
pub trait View: 'static + Sized {
    /// This view's identity, if it has one. A view typically holds the backing
    /// entity as a field and returns its [`EntityId`] here.
    ///
    /// The id becomes this view's [`ElementId`], so two views keyed on the same
    /// entity must not be rendered at the same position in the element tree
    /// (e.g. as siblings under the same parent): their internal element state
    /// (`use_state`, scroll offsets, etc.) would silently collide. Nesting is
    /// fine — the id is scoped by the parent path.
    fn entity_id(&self) -> Option<EntityId>;

    /// Render this view into an element tree, consuming `self`.
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement;
}

/// A stateless component (`RenderOnce`) is a `View` with no identity.
impl<T: RenderOnce> View for T {
    fn entity_id(&self) -> Option<EntityId> {
        None
    }

    #[inline]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        RenderOnce::render(self, window, cx)
    }
}

/// An entity that renders itself (`Render`) is a `View` keyed on its own id.
impl<T: Render> View for Entity<T> {
    fn entity_id(&self) -> Option<EntityId> {
        Some(Entity::entity_id(self))
    }

    #[inline]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.update(cx, |this, cx| {
            Render::render(this, window, cx).into_any_element()
        })
    }
}

impl<T: Render> Entity<T> {
    /// Embed this entity as a cached [`ViewElement`] laid out at `style`.
    ///
    /// The rendered subtree is reused until the entity is notified (or the
    /// cached bounds / text style change). Caching requires a definite size:
    /// a cached view is laid out from `style` and is *not* measured from its
    /// contents. Use [`ViewElement::new`] (or `.child(entity)`) for the
    /// uncached case.
    #[track_caller]
    pub fn cached(self, style: StyleRefinement) -> ViewElement<Entity<T>> {
        ViewElement::with_type_name(self, type_name::<T>()).cached(style)
    }

    /// Cache this fixed-size view, allowing vertical pixel-aligned replay while scrolling.
    ///
    /// Enable translation only for position-independent content without active selection or
    /// animation. Pass false after scrolling so descendants receive their final coordinates.
    /// Notify the entity when its content, styling, or externally supplied callbacks change.
    #[track_caller]
    pub fn cached_scroll(
        self,
        style: StyleRefinement,
        allow_translate: bool,
    ) -> ViewElement<Entity<T>> {
        let mut element = self.cached(style);
        element.scroll_cache = Some(allow_translate);
        element
    }
}

/// The element type for [`View`] implementations. Wraps a `View` and hooks it
/// into layout, prepaint, and paint. Constructed via [`ViewElement::new`].
#[doc(hidden)]
pub struct ViewElement<V: View> {
    view: Option<V>,
    entity_id: Option<EntityId>,
    cached_style: Option<StyleRefinement>,
    scroll_cache: Option<bool>,
    #[cfg(feature = "profiler")]
    view_type_name: &'static str,
    #[cfg(debug_assertions)]
    source: &'static core::panic::Location<'static>,
}

impl<V: View> ViewElement<V> {
    /// Wrap a [`View`] as an element.
    #[track_caller]
    pub fn new(view: V) -> Self {
        Self::with_type_name(view, type_name::<V>())
    }

    /// Wraps a view whose draw time profiling attributes to `view_type_name`,
    /// such as an entity's type rather than [`Entity`]'s.
    #[track_caller]
    #[cfg_attr(not(feature = "profiler"), expect(unused_variables))]
    pub(crate) fn with_type_name(view: V, view_type_name: &'static str) -> Self {
        let entity_id = view.entity_id();
        ViewElement {
            entity_id,
            cached_style: None,
            scroll_cache: None,
            #[cfg(feature = "profiler")]
            view_type_name,
            view: Some(view),
            #[cfg(debug_assertions)]
            source: core::panic::Location::caller(),
        }
    }

    /// Enable caching of this view's rendered subtree, laid out at `style`.
    /// The composer supplies the layout style because caching skips rendering
    /// the contents to measure them.
    ///
    /// Crate-private on purpose: caching is only sound for entity-backed views,
    /// where [`Context::notify`] is the contract that busts the cache. A stateless
    /// view has no such contract, so a frozen subtree could never be invalidated.
    /// Reach this through [`Entity::cached`] or [`AnyView::cached`], which are
    /// entity-backed by construction.
    pub(crate) fn cached(mut self, style: StyleRefinement) -> Self {
        self.cached_style = Some(style);
        self
    }
}

impl<V: View> IntoElement for ViewElement<V> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct ViewElementState {
    prepaint_range: Range<PrepaintStateIndex>,
    paint_range: Range<PaintIndex>,
    cache_key: ViewElementCacheKey,
    accessed_entities: FxHashSet<EntityId>,
    scroll: Option<ScrollSnapshot>,
}

// claudesktop patch: only the frame-local ranges advance after replay. The
// original origin, hitboxes and scene remain untouched until the next redraw.
struct ScrollSnapshot {
    origin: Point<Pixels>,
    scale: f32,
    rem: Pixels,
    opacity: f32,
    atlas_revision: u64,
    hitboxes: Vec<Hitbox>,
    scene: Option<RecordedScene>,
    translated: Option<ScrollTranslation>,
}

struct ScrollTranslation {
    from_recording: Pixels,
    from_previous_frame: Pixels,
}

struct ViewElementCacheKey {
    bounds: Bounds<Pixels>,
    content_mask: ContentMask<Pixels>,
    text_style: TextStyle,
}

impl<V: View> Element for ViewElement<V> {
    type RequestLayoutState = Option<AnyElement>;
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        self.entity_id.map(ElementId::View)
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        #[cfg(debug_assertions)]
        return Some(self.source);

        #[cfg(not(debug_assertions))]
        return None;
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        if let Some(entity_id) = self.entity_id {
            // Stateful path: create a reactive boundary.
            #[cfg(feature = "profiler")]
            let start = window.window_profiler.begin_view();
            let view = &mut self.view;
            let layout = request_layout_view(
                entity_id,
                self.cached_style.as_ref(),
                window,
                cx,
                &mut |window, cx| view.take().unwrap().render(window, cx).into_any_element(),
            );
            #[cfg(feature = "profiler")]
            window.window_profiler.end_view(
                start,
                self.view_type_name,
                crate::profiler::ViewPhase::RequestLayout,
            );
            layout
        } else {
            // Stateless path: isolate subtree via type name (no entity identity).
            request_layout_component(type_name::<V>(), window, cx, &mut |window, cx| {
                self.view
                    .take()
                    .unwrap()
                    .render(window, cx)
                    .into_any_element()
            })
        }
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        if let Some(entity_id) = self.entity_id {
            // Stateful path.
            #[cfg(feature = "profiler")]
            let start = window.window_profiler.begin_view();
            let prepainted = prepaint_view(
                entity_id,
                global_id,
                bounds,
                element,
                self.scroll_cache,
                window,
                cx,
                &mut |window, cx| {
                    self.view
                        .take()
                        .unwrap()
                        .render(window, cx)
                        .into_any_element()
                },
            );
            #[cfg(feature = "profiler")]
            window.window_profiler.end_view(
                start,
                self.view_type_name,
                crate::profiler::ViewPhase::Prepaint,
            );
            prepainted
        } else {
            // Stateless path: just prepaint the element.
            prepaint_component(type_name::<V>(), element, window, cx)
        }
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        element: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(entity_id) = self.entity_id {
            // Stateful path.
            #[cfg(feature = "profiler")]
            let start = window.window_profiler.begin_view();
            paint_view(
                entity_id,
                self.cached_style.is_some(),
                global_id,
                element,
                window,
                cx,
            );
            #[cfg(feature = "profiler")]
            window.window_profiler.end_view(
                start,
                self.view_type_name,
                crate::profiler::ViewPhase::Paint,
            );
        } else {
            // Stateless path: just paint the element.
            paint_component(std::any::type_name::<V>(), element, window, cx);
        }
    }
}

/// A view that renders nothing
pub struct EmptyView;

impl Render for EmptyView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[inline(never)]
fn request_layout_view(
    entity_id: EntityId,
    cached_style: Option<&StyleRefinement>,
    window: &mut Window,
    cx: &mut App,
    render: &mut dyn FnMut(&mut Window, &mut App) -> AnyElement,
) -> (LayoutId, Option<AnyElement>) {
    window.with_rendered_view(entity_id, |window| {
        let caching_disabled = window.is_inspector_picking(cx);
        match cached_style {
            Some(style) if !caching_disabled => {
                let mut root_style = Style::default();
                root_style.refine(style);
                let layout_id = window.request_layout(root_style, None, cx);
                (layout_id, None)
            }
            _ => {
                let mut element = render(window, cx);
                let layout_id = element.request_layout(window, cx);
                (layout_id, Some(element))
            }
        }
    })
}

#[inline(never)]
fn request_layout_component(
    name: &'static str,
    window: &mut Window,
    cx: &mut App,
    render: &mut dyn FnMut(&mut Window, &mut App) -> AnyElement,
) -> (LayoutId, Option<AnyElement>) {
    window.with_id(ElementId::from(name), |window| {
        let mut element = render(window, cx);
        let layout_id = element.request_layout(window, cx);
        (layout_id, Some(element))
    })
}

#[inline(never)]
fn prepaint_view(
    entity_id: EntityId,
    global_id: Option<&GlobalElementId>,
    bounds: Bounds<Pixels>,
    element: &mut Option<AnyElement>,
    scroll_cache: Option<bool>,
    window: &mut Window,
    cx: &mut App,
    render: &mut dyn FnMut(&mut Window, &mut App) -> AnyElement,
) -> Option<AnyElement> {
    window.set_view_id(entity_id);
    window.with_rendered_view(entity_id, |window| {
        if let Some(mut element) = element.take() {
            element.prepaint(window, cx);
            return Some(element);
        }

        window.with_element_state::<ViewElementState, _>(
            global_id.unwrap(),
            |element_state, window| {
                let content_mask = window.content_mask();
                let text_style = window.text_style();
                let scale = window.scale_factor();
                let rem = window.rem_size();
                let opacity = window.element_opacity();
                let atlas_revision = window.scroll_atlas_revision;
                let scroll_cache = scroll_cache.filter(|_| !window.scroll_prepaint_recording);

                if let Some(mut element_state) = element_state
                    && element_state.cache_key.text_style == text_style
                    && !window.dirty_views.contains(&entity_id)
                {
                    let snapshot = element_state.scroll.as_mut().filter(|snapshot| {
                        scroll_cache.is_some()
                            && !window.refreshing_window
                            && snapshot.scale == scale
                            && snapshot.rem == rem
                            && snapshot.opacity == opacity
                            && snapshot.atlas_revision == atlas_revision
                    });
                    let stationary = element_state.cache_key.bounds == bounds
                        && element_state.cache_key.content_mask == content_mask
                        && if let Some(snapshot) = snapshot.as_ref() {
                            snapshot.translated.is_none()
                        } else {
                            scroll_cache.is_none() && !window.refreshing
                        };
                    let prepaint_start = window.prepaint_index();
                    let reused = if stationary {
                        window.reuse_prepaint(element_state.prepaint_range.clone());
                        true
                    } else if let Some(snapshot) = snapshot
                        && scroll_cache == Some(true)
                        && bounds.origin.x == snapshot.origin.x
                        && bounds.size == element_state.cache_key.bounds.size
                        && snapshot
                            .scene
                            .as_ref()
                            .is_some_and(|scene| !scene.has_non_unit_transform)
                    {
                        let dy = bounds.origin.y - snapshot.origin.y;
                        let physical = f32::from(dy) * scale;
                        if (physical - physical.round()).abs() < 0.02 {
                            window.reuse_prepaint_translated(
                                &snapshot.hitboxes,
                                element_state.prepaint_range.clone(),
                                dy,
                                content_mask,
                            );
                            let previous_dy = snapshot
                                .translated
                                .as_ref()
                                .map_or(Pixels::ZERO, |translation| translation.from_recording);
                            snapshot.translated = Some(ScrollTranslation {
                                from_recording: dy,
                                from_previous_frame: dy - previous_dy,
                            });
                            element_state.cache_key.bounds = bounds;
                            element_state.cache_key.content_mask = content_mask;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    if reused {
                        cx.entities
                            .extend_accessed(&element_state.accessed_entities);
                        element_state.prepaint_range = prepaint_start..window.prepaint_index();
                        return (None, element_state);
                    }
                }

                let refreshing = mem::replace(&mut window.refreshing, true);
                let prepaint_start = window.prepaint_index();
                let ((element, hitboxes), accessed_entities) = cx.detect_accessed_entities(|cx| {
                    let mut element = render(window, cx);
                    element.layout_as_root(Size::<AvailableSpace>::from(bounds.size), window, cx);
                    let hitboxes = if scroll_cache.is_some() {
                        window
                            .record_scroll_prepaint(|window| {
                                element.prepaint_at(bounds.origin, window, cx);
                            })
                            .1
                    } else {
                        element.prepaint_at(bounds.origin, window, cx);
                        None
                    };
                    (element, hitboxes)
                });

                let prepaint_end = window.prepaint_index();
                window.refreshing = refreshing;

                (
                    Some(element),
                    ViewElementState {
                        accessed_entities,
                        prepaint_range: prepaint_start..prepaint_end,
                        paint_range: PaintIndex::default()..PaintIndex::default(),
                        scroll: hitboxes.map(|hitboxes| ScrollSnapshot {
                            origin: bounds.origin,
                            scale,
                            rem,
                            opacity,
                            atlas_revision,
                            hitboxes,
                            scene: None,
                            translated: None,
                        }),
                        cache_key: ViewElementCacheKey {
                            bounds,
                            content_mask,
                            text_style,
                        },
                    },
                )
            },
        )
    })
}

#[inline(never)]
fn prepaint_component(
    name: &'static str,
    element: &mut Option<AnyElement>,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    window.with_id(ElementId::from(name), |window| {
        element.as_mut().unwrap().prepaint(window, cx);
    });
    Some(element.take().unwrap())
}

#[inline(never)]
fn paint_view(
    entity_id: EntityId,
    cached: bool,
    global_id: Option<&GlobalElementId>,
    element: &mut Option<AnyElement>,
    window: &mut Window,
    cx: &mut App,
) {
    window.with_rendered_view(entity_id, |window| {
        let caching_disabled = window.is_inspector_picking(cx);
        if cached && !caching_disabled {
            window.with_element_state::<ViewElementState, _>(
                global_id.unwrap(),
                |element_state, window| {
                    let mut element_state = element_state.unwrap();

                    let paint_start = window.paint_index();

                    if let Some(element) = element {
                        let refreshing = mem::replace(&mut window.refreshing, true);
                        if let Some(scroll) = &mut element_state.scroll {
                            scroll.scene = Some(window.record_scroll_paint(|window| {
                                element.paint(window, cx);
                            }));
                        } else {
                            element.paint(window, cx);
                        }
                        window.refreshing = refreshing;
                    } else if let Some(scroll) = &element_state.scroll
                        && let Some(translation) = &scroll.translated
                    {
                        window.reuse_paint_translated(
                            scroll.scene.as_ref().unwrap(),
                            element_state.paint_range.clone(),
                            translation.from_recording,
                            translation.from_previous_frame,
                            element_state.cache_key.content_mask,
                        );
                    } else {
                        window.reuse_paint(element_state.paint_range.clone());
                    }

                    let paint_end = window.paint_index();
                    element_state.paint_range = paint_start..paint_end;

                    ((), element_state)
                },
            )
        } else {
            element.as_mut().unwrap().paint(window, cx);
        }
    });
}

#[inline(never)]
fn paint_component(
    name: &'static str,
    element: &mut Option<AnyElement>,
    window: &mut Window,
    cx: &mut App,
) {
    window.with_id(ElementId::Name(name.into()), |window| {
        element.as_mut().unwrap().paint(window, cx);
    });
}

#[cfg(test)]
mod scroll_cache_tests {
    use crate::{
        self as gpui, AppContext, Context, Entity, InteractiveElement as _, IntoElement,
        ListAlignment, ListOffset, ListState, Modifiers, MouseButton, ParentElement as _, Pixels,
        Render, ScrollDelta, ScrollWheelEvent, StyleRefinement, Styled as _, TestAppContext,
        TouchPhase, Window, canvas, div, fill, list, point, px, rgb,
    };
    use std::{cell::Cell, rc::Rc};

    struct Body {
        rendered: Rc<Cell<usize>>,
        top: Rc<Cell<Pixels>>,
        clicked_at: Rc<Cell<Option<Pixels>>>,
    }

    impl Render for Body {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.rendered.set(self.rendered.get() + 1);
            let top = self.top.clone();
            let clicked_at = self.clicked_at.clone();
            let paint = canvas(
                move |bounds, _, _| top.set(bounds.top()),
                |bounds, _, window, _| window.paint_quad(fill(bounds, rgb(0x524321))),
            )
            .h(px(20.))
            .w_full();
            div()
                .size_full()
                .on_mouse_down(MouseButton::Left, {
                    let top = self.top.clone();
                    move |_, _, _| clicked_at.set(Some(top.get()))
                })
                .child(paint)
        }
    }

    struct Scroller {
        body: Entity<Body>,
        top: Pixels,
        moving: bool,
    }

    impl Render for Scroller {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .relative()
                .w(px(120.))
                .h(px(80.))
                .overflow_hidden()
                .child(div().absolute().top(self.top).left_0().child(
                    self.body.clone().cached_scroll(
                        StyleRefinement::default().w(px(100.)).h(px(60.)),
                        self.moving,
                    ),
                ))
        }
    }

    #[gpui::test]
    fn cached_scroll_replays_then_settles_before_pointer_dispatch(cx: &mut TestAppContext) {
        let rendered = Rc::new(Cell::new(0));
        let top = Rc::new(Cell::new(px(0.)));
        let clicked_at = Rc::new(Cell::new(None));
        let (root, cx) = cx.add_window_view(|_, cx| Scroller {
            body: cx.new(|_| Body {
                rendered: rendered.clone(),
                top: top.clone(),
                clicked_at: clicked_at.clone(),
            }),
            top: px(-15.),
            moving: false,
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(top.get(), px(-15.));
        let initial = rendered.get();

        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.top = px(15.);
                root.moving = true;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert_eq!(rendered.get(), initial);
        assert_eq!(top.get(), px(-15.));
        for (x, y) in [(0.25, 7.), (-1., -9.), (5., 1.)] {
            cx.simulate_event(ScrollWheelEvent {
                position: point(px(5.), px(20.)),
                delta: ScrollDelta::Pixels(point(px(x), px(y))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            });
        }
        assert_eq!(
            rendered.get(),
            initial,
            "diagonal input must not settle the window"
        );
        cx.simulate_mouse_down(
            point(px(5.), px(20.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        assert_eq!(clicked_at.get(), Some(px(15.)));
        assert!(rendered.get() > initial);

        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.top = px(25.);
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert_eq!(top.get(), px(15.));
        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.moving = false;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert_eq!(top.get(), px(25.));

        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.top = px(35.);
                root.moving = true;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert_eq!(top.get(), px(25.));
        cx.simulate_event(ScrollWheelEvent {
            position: point(px(5.), px(40.)),
            delta: ScrollDelta::Pixels(point(px(12.), px(0.))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        assert_eq!(
            top.get(),
            px(35.),
            "horizontal scroll must settle before dispatch"
        );

        let before_force = rendered.get();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert_eq!(rendered.get(), before_force + 1);
    }

    struct CachedList {
        state: ListState,
        rows: Vec<Entity<Body>>,
    }

    impl Render for CachedList {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let rows = self.rows.clone();
            list(self.state.clone(), move |ix, _, _| {
                rows[ix]
                    .clone()
                    .cached_scroll(StyleRefinement::default().w_full().h(px(22.25)), true)
                    .into_any_element()
            })
            .w(px(120.))
            .h(px(64.))
        }
    }

    #[gpui::test]
    fn cached_scroll_preserves_device_phase_across_top_item_at_150_percent(
        cx: &mut TestAppContext,
    ) {
        let state = ListState::new(6, ListAlignment::Top, px(0.));
        let counts = (0..6).map(|_| Rc::new(Cell::new(0))).collect::<Vec<_>>();
        let (_, cx) = cx.add_window_view(|window, cx| {
            window.set_scale_factor(1.5);
            state.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(21.),
            });
            CachedList {
                state: state.clone(),
                rows: counts
                    .iter()
                    .map(|count| {
                        cx.new(|_| Body {
                            rendered: count.clone(),
                            top: Rc::new(Cell::new(px(0.))),
                            clicked_at: Rc::new(Cell::new(None)),
                        })
                    })
                    .collect(),
            }
        });

        let first_painted_top = |window: &Window| {
            window
                .rendered_frame
                .scene
                .quads
                .iter()
                .map(|quad| quad.bounds.top().0)
                .min_by(f32::total_cmp)
                .unwrap()
        };
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert_eq!(state.logical_scroll_top().item_ix, 0);
            assert_eq!(first_painted_top(window), 2.);
        });
        let retained_rows = [counts[1].get(), counts[2].get()];
        cx.update(|window, cx| {
            state.scroll_by(px(2.)); // Three device pixels, crossing item 0's bottom.
            window.draw(cx).clear(cx);
            assert_eq!(state.logical_scroll_top().item_ix, 1);
            assert_eq!(first_painted_top(window), -1.);
        });
        assert_eq!([counts[1].get(), counts[2].get()], retained_rows);
    }
}
