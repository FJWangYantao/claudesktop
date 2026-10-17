//! 动效：限帧的转圈 / 扫光、展开收起（弹簧 + 高度裁切）、悬停淡入、新内容入场、平滑回到底部。
//!
//! gpui 的动画每一帧都要重画它所在的整个 view（`request_animation_frame` 通知当前 view，
//! 祖先一并变脏），所以常驻的循环动画（转圈、扫光）不用 `with_animation`：相位按全局时钟算，
//! 由一个共用的节拍器每隔约 33ms（60Hz 下每两帧）让画着它们的 view 重画一次（[`request_loop_tick`]）。
//! 不用 `Animation::with_max_fps`：它靠计时器排下一帧，Windows 计时器按 15.6ms 一档取整，
//! 再等一次垂直同步，实测只剩 21~25 帧，还和别的动画错开、多出重画。
//! 一次性的过渡（展开、淡入、侧栏）很短，满帧跑。时长和曲线取 gpui-kit 主题的 motion tokens
//! （120 / 180 / 280ms，ease-out，弹簧不回弹）。全部遵守「减少动态效果」：
//! 系统设置或设置页的「动画」关掉后直接落到终态（见 [`ChatApp::apply_motion_pref`]）。

use super::ChatApp;
use crate::settings::MotionPref;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::base::animation::ease_out_cubic;
use gpui_kit::base::motion::{Transition, spring, transition};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::ops::Range;
use std::panic::Location;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 循环动画两次重画之间至少隔这么久（约 30 帧）
const LOOP_INTERVAL: Duration = Duration::from_millis(33);
/// 帧到来的时刻有抖动：差这么一点也算到了（60Hz 下正好每两帧一拍）
const TICK_SLACK: Duration = Duration::from_millis(4);
/// 转一圈的时长（gpui-kit `Spinner` 同款）
const SPIN_PERIOD: Duration = Duration::from_millis(800);
/// 扫光扫一遍的时长
const SHIMMER_PERIOD: Duration = Duration::from_millis(1600);

/// `App::reduce_motion` 的镜像：画转圈 / 扫光时手里没有 `App`（由 [`ChatApp::apply_motion_pref`] 同步）
static REDUCED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// 这一拍到点时要重画的 view（非空 = 已经排了一拍、还没到点）
    static TICK_VIEWS: RefCell<Vec<EntityId>> = const { RefCell::new(Vec::new()) };
}

/// 循环动画在这一刻的相位（0..1），所有转圈 / 扫光共用一个时钟，彼此同步。
fn loop_phase(period: Duration) -> f32 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let t = EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64();
    (t % period.as_secs_f64() / period.as_secs_f64()) as f32
}

/// 约 33ms 后让当前 view 重画一次（prepaint 里调）。各个 view 共用一拍：同一拍里
/// 多次调用只排一次，到点时把这期间登记过的 view 一起通知（同一帧里重画）。
/// 靠 `on_next_frame` 逐帧数时间，不走计时器：拍子落在垂直同步上，和别的重画对齐。
fn request_loop_tick(window: &mut Window) {
    if REDUCED.load(Ordering::Relaxed) {
        return;
    }
    let view = window.current_view();
    let first = TICK_VIEWS.with_borrow_mut(|views| {
        let first = views.is_empty();
        if !views.contains(&view) {
            views.push(view);
        }
        first
    });
    if first {
        wait_for_tick(Instant::now(), window);
    }
}

fn wait_for_tick(since: Instant, window: &Window) {
    window.on_next_frame(move |window, cx| {
        if since.elapsed() + TICK_SLACK >= LOOP_INTERVAL {
            for view in TICK_VIEWS.take() {
                cx.notify(view);
            }
        } else {
            wait_for_tick(since, window);
        }
    });
}

/// 不占地方、不画东西，只在 prepaint 时排下一拍（循环动画画在哪就放在哪）。
fn ticker() -> impl IntoElement {
    canvas(|_, window, _| request_loop_tick(window), |_, _, _, _| {}).absolute().size_0()
}

/// 扫光高光的半宽（占整段文字的比例）
const SHIMMER_SPREAD: f32 = 0.35;

/// 新内容入场：淡入的同时从下方这么远的地方升上来
const ENTER_RISE: Pixels = px(8.);

/// 平滑回到底部：剩下的距离每过这么久衰减到 1/e（约 4 倍时长基本到底）
pub(super) const JUMP_TAU: f32 = 0.06;
/// 平滑回到底部最多滑这么久，之后直接交给自动跟随（流式输出时底部一直在长）
pub(super) const JUMP_MAX: Duration = Duration::from_millis(500);

/// 「回到底部」的平滑滚动进行中：哪个会话、何时开始、上一帧的时刻。
#[derive(Clone, Copy)]
pub(super) struct JumpAnim {
    pub token: u64,
    pub started: Instant,
    pub last: Instant,
}

/// 转圈（赤陶色，12px）：gpui-kit `Spinner` 的同款图标和节奏，约 30 帧。
pub(super) fn spinner() -> Div {
    let reduced = REDUCED.load(Ordering::Relaxed);
    let turn = if reduced { 0. } else { ease_in_out(loop_phase(SPIN_PERIOD)) };
    div()
        .flex_shrink_0()
        .child(
            Icon::new(IconName::Loader)
                .xsmall()
                .text_color(rgb(CORAL))
                .transform(Transformation::rotate(percentage(turn))),
        )
        .when(!reduced, |d| d.child(ticker()))
}

/// 扫光文字（「思考中…」）：`base` 色的字上一道 `highlight` 色的高光从左扫到右，约 30 帧。
/// 按字着色（`StyledText` 高亮），减少动态效果时整段都是 `base` 色。
pub(super) fn shimmer_text(text: impl Into<SharedString>, base: Color, highlight: Color) -> Div {
    let text: SharedString = text.into();
    let reduced = REDUCED.load(Ordering::Relaxed);
    let (base, highlight) = (rgb(base), rgb(highlight));
    let n = text.chars().count().max(1) as f32;
    // 高光中心从左边外侧走到右边外侧，首尾都不露半截；减少动态效果时停在左边外侧
    let phase = if reduced { 0. } else { loop_phase(SHIMMER_PERIOD) };
    let center = -SHIMMER_SPREAD + phase * (1. + 2. * SHIMMER_SPREAD);
    let highlights: Vec<(Range<usize>, HighlightStyle)> = text
        .char_indices()
        .enumerate()
        .map(|(i, (at, c))| {
            let x = (i as f32 + 0.5) / n;
            let k = (1. - (x - center).abs() / SHIMMER_SPREAD).max(0.);
            let color = mix(base, highlight, k);
            (
                at..at + c.len_utf8(),
                HighlightStyle { color: Some(color.into()), ..Default::default() },
            )
        })
        .collect();
    div()
        .relative()
        .child(StyledText::new(text).with_highlights(highlights))
        .when(!reduced, |d| d.child(ticker()))
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// 展开进度：打开时弹簧走向 1，收起时走回 0（走完前仍大于 0，内容要继续画）。
/// `id` 要在同一行里唯一；减少动态效果时直接是 0 或 1。
pub(super) fn reveal_progress(
    id: impl Into<ElementId>,
    open: bool,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let policy = cx.theme().motion_tokens().spring_control;
    spring((id.into(), "reveal"), if open { 1f32 } else { 0. }, policy, window, cx)
}

/// 悬停才出现的操作的透明度：120ms 淡入 / 淡出（不挤布局，只改透明度）。
pub(super) fn hover_opacity(
    id: impl Into<ElementId>,
    hovered: bool,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let policy = Transition::new(cx.theme().motion_tokens().duration_fast);
    transition((id.into(), "hover"), if hovered { 1f32 } else { 0. }, policy, window, cx)
}

/// 刚出现的东西（刚发出的消息、刚变成对勾的「已复制」）入场：出现了 `age`，
/// 还在 180ms 内时返回（透明度，下移量）并要下一帧；走完或减少动态效果时为 None。
pub(super) fn enter(age: Duration, window: &mut Window, cx: &App) -> Option<(f32, Pixels)> {
    let duration = cx.theme().motion_tokens().duration_normal;
    if cx.reduce_motion() || age >= duration {
        return None;
    }
    window.request_animation_frame();
    let t = ease_out_cubic(age.as_secs_f32() / duration.as_secs_f32());
    Some((t, ENTER_RISE * (1. - t)))
}

/// 一段带入场动画的内容：`started` 是它出现的时刻（None = 不动）。
pub(super) fn entering(
    el: impl IntoElement,
    started: Option<Instant>,
    window: &mut Window,
    cx: &App,
) -> Div {
    let wrap = div().child(el);
    match started.and_then(|t| enter(t.elapsed(), window, cx)) {
        Some((opacity, rise)) => wrap.relative().top(rise).opacity(opacity),
        None => wrap,
    }
}

/// 展开 / 收起的高度动画：`progress` 1 = 完全展开（按内容正常排版），0 = 收起，
/// 中间按上次量到的内容高度乘进度裁切。
///
/// 和 gpui-base 的 `MotionReveal` 的区别：完全展开时不接管排版，所以一行首次出现
/// （滚进视口、切会话）不会先塌成 0 高一帧——在虚拟列表里那会让滚动位置跳一下。
pub(super) struct Reveal {
    id: ElementId,
    progress: f32,
    child: AnyElement,
}

impl Reveal {
    pub(super) fn new(id: impl Into<ElementId>, progress: f32, child: impl IntoElement) -> Self {
        Self { id: id.into(), progress: progress.clamp(0., 1.), child: child.into_any_element() }
    }
}

#[derive(Clone, Copy, Default)]
struct RevealState {
    height: Option<Pixels>,
}

impl IntoElement for Reveal {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Reveal {
    /// true = 裁切中（子元素单独排版，自己的高度按进度给）
    type RequestLayoutState = bool;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, bool) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if self.progress >= 1. {
            let child = self.child.request_layout(window, cx);
            return (window.request_layout(style, [child], cx), false);
        }
        let id = global_id.expect("Reveal 有 id");
        let height = window.with_element_state(id, |state: Option<RevealState>, _| {
            let state = state.unwrap_or_default();
            (state.height, state)
        });
        // 刚开始展开、还没量过：先按 0 高，量完下一帧就有了（弹簧本来就在要帧）
        style.size.height = (height.unwrap_or_default() * self.progress).into();
        (window.request_layout(style, None, cx), true)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        clipped: &mut bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        let height = if *clipped {
            let measured = self.child.layout_as_root(
                size(AvailableSpace::Definite(bounds.size.width), AvailableSpace::MinContent),
                window,
                cx,
            );
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                self.child.prepaint_at(bounds.origin, window, cx);
            });
            measured.height
        } else {
            self.child.prepaint(window, cx);
            bounds.size.height
        };
        let id = global_id.expect("Reveal 有 id");
        window.with_element_state(id, |_: Option<RevealState>, _| {
            ((), RevealState { height: Some(height) })
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        clipped: &mut bool,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if *clipped {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                self.child.paint(window, cx);
            });
        } else {
            self.child.paint(window, cx);
        }
    }
}

impl ChatApp {
    /// 按设置页的「动画」写进 gpui 的减少动态效果开关：跟随系统 / 总是播放 / 关闭。
    /// 「跟随系统」时重读系统设置（Windows 改了不通知，窗口重新激活时也调一次）。
    pub(crate) fn apply_motion_pref(&mut self, cx: &mut App) {
        match self.settings.motion {
            MotionPref::On => cx.set_reduce_motion(false),
            MotionPref::Off => cx.set_reduce_motion(true),
            MotionPref::System => {
                // gpui-base 只在开关还是它上次写的值时才肯覆盖：先还原成那个值再让它重读
                cx.set_reduce_motion(self.system_reduce_motion);
                gpui_kit::base::apply_system_reduce_motion(cx);
                self.system_reduce_motion = cx.reduce_motion();
            }
        }
        REDUCED.store(cx.reduce_motion(), Ordering::Relaxed);
    }
}
