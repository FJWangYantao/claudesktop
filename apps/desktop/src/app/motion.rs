//! 动效：限帧的转圈 / 呼吸圆点 / 扫光、展开收起（弹簧 + 高度裁切）、悬停淡入、新内容入场、平滑回到底部。
//!
//! 「进行中」只有两种样子：通用的圆弧转圈（[`spinner`]）和运行状态行专用的呼吸圆点（[`breathing_dot`]），
//! 都用 `canvas` + `PathBuilder` 矢量画，不靠旋转图标或换字形（小尺寸下旋转的位图发虚、
//! 符号字体里各个字形粗细不一）。
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
use gpui_kit::*;
use std::cell::RefCell;
use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::panic::Location;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 循环动画两次重画之间至少隔这么久（约 30 帧）
const LOOP_INTERVAL: Duration = Duration::from_millis(33);
/// 帧到来的时刻有抖动：差这么一点也算到了（60Hz 下正好每两帧一拍）
const TICK_SLACK: Duration = Duration::from_millis(4);
// 循环动画的节奏：转圈 1 秒一圈，圆弧伸缩和扫光 2 秒一拍，状态行圆点 1.6 秒一呼一吸
/// 转圈匀速转一圈的时长
const SPIN_PERIOD: Duration = Duration::from_millis(1000);
/// 圆弧一伸一缩的周期
const PULSE_PERIOD: Duration = Duration::from_millis(2000);
/// 状态行圆点变大变亮再回落的周期
const BREATH_PERIOD: Duration = Duration::from_millis(1600);
/// 扫光扫一遍的时长
const SHIMMER_PERIOD: Duration = Duration::from_millis(2000);

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

/// 扫光高光的半宽（按字号算，约两个半字宽）
const SHIMMER_SPREAD_EM: f32 = 2.5;
/// 扫光的颜色从底色到高光分这么多档（每档一条竖向裁切区，档差小到看不出台阶）
const SHIMMER_LEVELS: usize = 10;

/// 新内容入场：淡入的同时从下方这么远的地方升上来
const ENTER_RISE: Pixels = px(8.);

/// 平滑回到底部：剩下的距离每过这么久衰减到 1/e（约 4 倍时长基本到底）
pub(super) const JUMP_TAU: f32 = 0.06;
/// 平滑回到底部最多滑这么久，之后直接交给自动跟随（流式输出时底部一直在长）
pub(super) const JUMP_MAX: Duration = Duration::from_millis(500);

/// 滚轮平滑：剩下的距离每过这么久衰减到 1/e（比回到底部干脆，滚轮要跟手）
pub(super) const WHEEL_TAU: f32 = 0.05;
/// 一串滚轮最后一格之后最多再滑这么久（正常 4 倍 TAU 就到了，这是兜底）
pub(super) const WHEEL_MAX: Duration = Duration::from_millis(600);

/// 「回到底部」的平滑滚动进行中：哪个会话、何时开始、上一帧的时刻。
#[derive(Clone, Copy)]
pub(super) struct JumpAnim {
    pub token: u64,
    pub started: Instant,
    pub last: Instant,
}

/// 转圈（赤陶色，12px，界面里所有「进行中」共用这一个：工具行、侧栏、压缩上下文）：
/// 淡色圆环上一段圆头圆弧匀速转，弧长随 [`PULSE_PERIOD`] 一伸一缩，矢量画（不是旋转图标，边缘不发虚）。
/// 减少动态效果时停成一段静止的四分之三圆弧。
pub(super) fn spinner() -> Div {
    let reduced = REDUCED.load(Ordering::Relaxed);
    // 起点和弧长都按圈数算
    let (start, sweep) = if reduced {
        (0., 0.75)
    } else {
        (loop_phase(SPIN_PERIOD), 0.2 + 0.45 * pulse(loop_phase(PULSE_PERIOD)))
    };
    let color = rgb(CORAL);
    div().size_3().flex_shrink_0().child(
        canvas(
            move |_, window, _| {
                if !reduced {
                    request_loop_tick(window);
                }
            },
            move |bounds, _, window, _| paint_spinner(bounds, start, sweep, color, window),
        )
        .size_full(),
    )
}

/// 0..1 的相位变成 0→1→0 的平滑起伏（余弦）。
fn pulse(phase: f32) -> f32 {
    0.5 - 0.5 * (phase * TAU).cos()
}

/// 圆周上第 `turns` 圈处的点（0 = 正上方，顺时针）。
fn on_circle(center: Point<Pixels>, r: Pixels, turns: f32) -> Point<Pixels> {
    let a = turns * TAU - FRAC_PI_2;
    center + point(r * a.cos(), r * a.sin())
}

/// 实心圆点（圆弧的圆头、星芒的花心）。
fn paint_dot(center: Point<Pixels>, r: Pixels, color: Rgba, window: &mut Window) {
    let (left, right) = (center - point(r, px(0.)), center + point(r, px(0.)));
    let mut dot = PathBuilder::fill();
    dot.move_to(right);
    dot.arc_to(point(r, r), px(0.), false, true, left);
    dot.arc_to(point(r, r), px(0.), false, true, right);
    dot.close();
    if let Ok(path) = dot.build() {
        window.paint_path(path, color);
    }
}

fn paint_spinner(bounds: Bounds<Pixels>, start: f32, sweep: f32, color: Rgba, window: &mut Window) {
    let d = bounds.size.width.min(bounds.size.height);
    let stroke = d * 0.15;
    let r = (d - stroke) / 2.;
    let c = bounds.center();
    // 轨道：同色的淡圆环，弧短的时候也看得出是个圈
    let mut ring = PathBuilder::stroke(stroke);
    ring.move_to(on_circle(c, r, 0.));
    ring.arc_to(point(r, r), px(0.), false, true, on_circle(c, r, 0.5));
    ring.arc_to(point(r, r), px(0.), false, true, on_circle(c, r, 0.));
    if let Ok(path) = ring.build() {
        window.paint_path(path, Rgba { a: 0.2, ..color });
    }
    let (from, to) = (on_circle(c, r, start), on_circle(c, r, start + sweep));
    let mut arc = PathBuilder::stroke(stroke);
    arc.move_to(from);
    arc.arc_to(point(r, r), px(0.), sweep > 0.5, true, to);
    if let Ok(path) = arc.build() {
        window.paint_path(path, color);
    }
    // 描边是平头：两端各补一个半个线宽的圆点当圆头
    paint_dot(from, stroke / 2., color, window);
    paint_dot(to, stroke / 2., color, window);
}

/// 运行状态行的等待动效（14px 格子）：一个赤陶色圆点缓慢变大变亮再回落，约 30 帧（节拍器重画）。
/// 早先试过换字形的 ✻ 和矢量星芒，小尺寸下都显得怪，最后选了最克制的圆点（对比稿见设计指南第 13 条）。
/// 减少动态效果时停在中间大小、不透明。
pub(super) fn breathing_dot() -> Div {
    let reduced = REDUCED.load(Ordering::Relaxed);
    let k = if reduced { 0.5 } else { pulse(loop_phase(BREATH_PERIOD)) };
    let color = rgb(CORAL);
    div().size_3p5().flex_shrink_0().child(
        canvas(
            move |_, window, _| {
                if !reduced {
                    request_loop_tick(window);
                }
            },
            move |bounds, _, window, _| {
                let r = bounds.size.width.min(bounds.size.height) / 2. * 0.92;
                let alpha = if reduced { 1. } else { 0.55 + 0.45 * k };
                paint_dot(
                    bounds.center(),
                    r * (0.38 + 0.14 * k),
                    Rgba { a: alpha, ..color },
                    window,
                );
            },
        )
        .size_full(),
    )
}

/// 扫光文字（「思考中…」）：`base` 色的字上一道 `highlight` 色的柔和高光从左扫到右，约 30 帧。
/// 按像素位置着色：高光强度是余弦钟形，切成 [`SHIMMER_LEVELS`] 档同心竖条，每档用一种颜色
/// 画一遍文字、裁到自己那条（各条不重叠，字形边缘不会被叠画加粗）。排版靠一份透明的同样文字。
/// 减少动态效果时整段都是 `base` 色。
pub(super) fn shimmer_text(text: impl Into<SharedString>, base: Color, highlight: Color) -> Div {
    let text: SharedString = text.into();
    if REDUCED.load(Ordering::Relaxed) {
        return div().text_color(rgb(base)).child(text);
    }
    let (base, highlight) = (rgb(base), rgb(highlight));
    let phase = loop_phase(SHIMMER_PERIOD);
    div().relative().child(div().text_color(transparent_black()).child(text.clone())).child(
        canvas(
            |_, window, _| request_loop_tick(window),
            move |bounds, _, window, cx| {
                paint_shimmer(&text, bounds, phase, base, highlight, window, cx)
            },
        )
        .absolute()
        .inset_0(),
    )
}

fn paint_shimmer(
    text: &SharedString,
    bounds: Bounds<Pixels>,
    phase: f32,
    base: Rgba,
    highlight: Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    // 字体、字号、行高跟外层排版那份文字一样（都继承同一串祖先的文字样式）
    let style = window.text_style();
    let rem = window.rem_size();
    let font_size = style.font_size.to_pixels(rem);
    let line_height = style.line_height_in_pixels(rem);
    let spread = font_size * SHIMMER_SPREAD_EM;
    // 高光中心从左边外侧走到右边外侧，首尾都不露半截
    let center = bounds.left() - spread + (bounds.size.width + spread * 2.) * phase;
    // 第 j 档的外沿：强度 0.5 + 0.5·cos(π·d/spread) 等于 j/档数 处离中心的距离
    let edge = |j: usize| spread * ((2. * j as f32 / SHIMMER_LEVELS as f32 - 1.).acos() / PI);
    // 上下多留一点，字形出了行框也不被切
    let (top, bottom) = (bounds.top() - px(4.), bounds.bottom() + px(4.));
    let (far_left, far_right) = (bounds.left() - px(4.), bounds.right() + px(4.));
    for j in 0..SHIMMER_LEVELS {
        let color = mix(base, highlight, j as f32 / (SHIMMER_LEVELS - 1) as f32);
        // 这一档占的横向范围：最外档是高光以外的全部，最内档是中间一整段，其余左右各一条
        let (outer, inner) = (edge(j), edge(j + 1));
        let spans = if j == 0 {
            [(far_left, center - inner), (center + inner, far_right)]
        } else if j == SHIMMER_LEVELS - 1 {
            [(center - outer, center + outer), (px(0.), px(0.))]
        } else {
            [(center - outer, center - inner), (center + inner, center + outer)]
        };
        let spans: Vec<_> = spans
            .into_iter()
            .map(|(l, r)| (l.max(far_left), r.min(far_right)))
            .filter(|(l, r)| r > l)
            .collect();
        if spans.is_empty() {
            continue;
        }
        let mut run = style.to_run(text.len());
        run.color = color.into();
        let line = window.text_system().shape_line(text.clone(), font_size, &[run], None);
        for (l, r) in spans {
            let mask =
                ContentMask { bounds: Bounds::from_corners(point(l, top), point(r, bottom)) };
            window.with_content_mask(Some(mask), |window| {
                let _ = line.paint(bounds.origin, line_height, TextAlign::Left, None, window, cx);
            });
        }
    }
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

/// 进行中的东西有了结果（转圈换成结果图标）的入场进度：从 0 走到 1，180ms ease-out。
/// 第一次画时就已经有结果（读历史、滚进视口）直接是 1，不补放。
pub(super) fn settle(
    id: impl Into<ElementId>,
    done: bool,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let policy = Transition::new(cx.theme().motion_tokens().duration_normal);
    transition((id.into(), "settle"), if done { 1f32 } else { 0. }, policy, window, cx)
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
