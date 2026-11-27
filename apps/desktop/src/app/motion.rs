//! 动效：限帧的转圈 / 几何状态指示 / 扫光、展开收起（弹簧 + 高度裁切）、悬停淡入、新内容入场、平滑回到底部。
//!
//! 「进行中」使用统一的弹性双块（[`spinner`]，`canvas` + `paint_quad` 画两块圆角方块）和少量线性状态图标。
//!
//! gpui 的动画每一帧都要重画它所在的整个 view（`request_animation_frame` 通知当前 view，
//! 祖先一并变脏），所以常驻的循环动画（转圈、扫光）不用 `with_animation`：相位按全局时钟算，
//! 由一个共用的节拍器每隔约 16ms（60Hz 下一帧）让画着它们的 view 重画一次（[`request_loop_tick`]）。
//! 内容区（对话、设置页、子任务详情）里的循环动画也不画在内容区里：每拍重画内容区就是重排整页对话，
//! 长回复、代码块多了一拍十几毫秒，跑着的时候转圈就掉帧。内容区里只放占位格、记下位置，
//! 由叠在内容区上面的小 view [`LoopOverlay`] 画，节拍器只重画它（侧栏同理，见 sidebar.rs 文件头）。
//! 不用 `Animation::with_max_fps`：它靠计时器排下一帧，Windows 计时器按 15.6ms 一档取整，
//! 再等一次垂直同步，实测只剩 21~25 帧，还和别的动画错开、多出重画。
//! 一次性的过渡（展开、淡入、侧栏）很短，满帧跑。时长和曲线取 gpui-kit 主题的 motion tokens
//! （120 / 180 / 280ms，ease-out，弹簧不回弹）。全部遵守「减少动态效果」：
//! 系统设置或设置页的「动画」关掉后直接落到终态（见 [`ChatApp::apply_motion_pref`]）。

use super::ChatApp;
use super::mascot::Mood;
use crate::settings::MotionPref;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::animation::{ease_in_out_cubic, ease_out_cubic};
use gpui_kit::base::motion::{Transition, spring, transition};
use gpui_kit::component::*;
use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::panic::Location;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 循环动画两次重画之间至少隔这么久（约 60 帧）
const LOOP_INTERVAL: Duration = Duration::from_millis(16);
/// 帧到来的时刻有抖动：差这么一点也算到了
const TICK_SLACK: Duration = Duration::from_millis(2);
// 循环动画的节奏：双块 3.2 秒走完一圈，扫光 2 秒一拍
/// 「进行中」双块沿四条边走完一圈的时长（8 个半步，每步 400ms）
const SPIN_PERIOD: Duration = Duration::from_millis(3200);
/// 双块的几何，全部相对 `bounds` 内接正方形的边长 `d`
const TILE_MARGIN: f32 = 0.04;
/// 单块占活动区边长的比例（剩下的缝隙自动是 `1 - 2 * TILE_LEN`）
const TILE_LEN: f32 = 0.43;
/// 圆角占单块短边的比例
const TILE_RADIUS: f32 = 0.28;
/// 每个半步里用来位移的比例，剩下的是原地停顿（伸出去贴满、顿一下、收回落位）
const TILE_MOVE_RATIO: f32 = 0.78;
/// 副块的透明度：同色压暗一档，深浅两套色板都看得出前后
const TILE_SECONDARY_ALPHA: f32 = 0.52;
/// 一圈的半步数：四条边各伸一次、收一次
const TILE_STEPS: usize = 8;
/// 扫光扫一遍的时长
const SHIMMER_PERIOD: Duration = Duration::from_millis(2000);

/// `App::reduce_motion` 的镜像：画转圈 / 扫光时手里没有 `App`（由 [`ChatApp::apply_motion_pref`] 同步）
static REDUCED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// 这一拍到点时要重画的 view（非空 = 已经排了一拍、还没到点）
    static TICK_VIEWS: RefCell<Vec<EntityId>> = const { RefCell::new(Vec::new()) };
    /// 正在构建的那块内容区（[`with_loop_spots`] 期间有效）：占位格在这时候认领位置池。
    /// 认领发生在**构建**时而不是 prepaint 时——prepaint 在整棵树布局完才跑，
    /// 那时已经分不清这个格是哪个内容区建的了。
    static CURRENT_SPOTS: RefCell<Option<LoopSpots>> = const { RefCell::new(None) };
    /// 最近一次发出动画重画通知的时刻，所有登记的视图共用。
    static LAST_LOOP_TICK: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// 一块内容区记下的循环动画位置（画什么、画在哪、裁到哪；窗口坐标）。
///
/// 每块内容区一份：两个 `ChatPane` 同帧渲染时各记各的，谁也清不掉对方的转圈位置
/// （只有一块时行为和以前的全局表一模一样）。
#[derive(Clone, Default)]
pub(crate) struct LoopSpots(std::rc::Rc<RefCell<Vec<LoopSpot>>>);

impl LoopSpots {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// 同一面板的延迟行构建会多次进入这里，保留其他行已登记的位置。
    pub(super) fn with_current<T>(&self, build: impl FnOnce() -> T) -> T {
        let previous = CURRENT_SPOTS.with_borrow_mut(|current| current.replace(self.clone()));
        let out = build();
        CURRENT_SPOTS.with_borrow_mut(|current| *current = previous);
        out
    }

    fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    fn push(&self, spot: LoopSpot) {
        self.0.borrow_mut().push(spot);
    }

    fn borrow(&self) -> std::cell::Ref<'_, Vec<LoopSpot>> {
        self.0.borrow()
    }
}

/// 内容区里的一个循环动画。
struct LoopSpot {
    kind: LoopKind,
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
}

enum LoopKind {
    Spinner,
    /// 扫光文字：字体字号取占位那份透明文字的样式
    Shimmer {
        text: SharedString,
        style: Box<TextStyle>,
        base: Rgba,
        highlight: Rgba,
    },
}

/// 「减少动态效果」当前的值（画首页背景时手里不一定有 `App`）。
pub(super) fn reduced() -> bool {
    REDUCED.load(Ordering::Relaxed)
}

/// 内容区构建这一遍时把位置池切到自己这份（上一遍记的作废）：占位格在构建时认领它，
/// prepaint 时按认领到的池记位置。退出时还原外层那份，嵌套的内容区不会互相污染。
pub(super) fn with_loop_spots<T>(spots: &LoopSpots, build: impl FnOnce() -> T) -> T {
    spots.clear();
    spots.with_current(build)
}

/// 循环动画的占位格：prepaint 时把位置和裁切范围记下，交给 [`LoopOverlay`] 画。
fn loop_spot(kind: LoopKind) -> Canvas<()> {
    // 认领在这一刻：构建占位格的就是要画它的那块内容区
    let spots = CURRENT_SPOTS.with_borrow(Clone::clone);
    canvas(
        move |bounds, window, _| {
            let clip = window.content_mask().bounds;
            if let Some(spots) = &spots {
                spots.push(LoopSpot { kind, bounds, clip });
            }
        },
        |_, _, _, _| {},
    )
}

/// 内容区里循环动画的覆盖层：叠在内容区上、不挂事件，按占位格记下的位置画。
/// 它排在内容区后面，prepaint 时内容区这一帧的位置已经记好（滚动时不会慢一帧）；
/// 节拍器只通知它自己，内容区复用缓存。
pub(crate) struct LoopOverlay {
    spots: LoopSpots,
}

impl LoopOverlay {
    pub(crate) fn new(spots: LoopSpots) -> Self {
        Self { spots }
    }
}

impl Render for LoopOverlay {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let ticks = self.spots.clone();
        let paint = self.spots.clone();
        canvas(
            move |_, window, _| {
                if !ticks.is_empty() {
                    request_loop_tick(window);
                }
            },
            move |_, _, window, cx| paint_loop_spots(&paint, window, cx),
        )
        .absolute()
        .size_full()
    }
}

fn paint_loop_spots(spots: &LoopSpots, window: &mut Window, cx: &mut App) {
    let spin = loop_phase(SPIN_PERIOD);
    let shimmer = loop_phase(SHIMMER_PERIOD);
    let color = rgb(CORAL);
    let spots = spots.borrow();
    if spots.is_empty() {
        return;
    }
    for spot in spots.iter() {
        let mask = ContentMask { bounds: spot.clip };
        window.with_content_mask(Some(mask), |window| match &spot.kind {
            LoopKind::Spinner => paint_spinner(spot.bounds, Some(spin), color, window),
            LoopKind::Shimmer { text, style, base, highlight } => {
                paint_shimmer(text, style, spot.bounds, shimmer, *base, *highlight, window, cx)
            }
        });
    }
}

/// 循环动画在这一刻的相位（0..1），所有转圈 / 扫光共用一个时钟，彼此同步。
fn loop_phase(period: Duration) -> f32 {
    let t = loop_clock().as_secs_f64();
    (t % period.as_secs_f64() / period.as_secs_f64()) as f32
}

/// 循环动画共用的时钟：从第一次用到现在过了多久
fn loop_clock() -> Duration {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed()
}

/// 为当前 view 登记下一拍重画（prepaint 里调）。各个 view 共用一拍：同一拍里
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
        wait_for_tick(window);
    }
}

fn wait_for_tick(window: &Window) {
    window.on_next_frame(|window, cx| {
        // 在同一帧阶段比较时间：布局和绘制花掉的几毫秒不应让下一拍再等一帧。
        let now = Instant::now();
        if LAST_LOOP_TICK.get().is_none_or(|last| now - last + TICK_SLACK >= LOOP_INTERVAL) {
            LAST_LOOP_TICK.set(Some(now));
            for view in TICK_VIEWS.take() {
                cx.notify(view);
            }
        } else {
            wait_for_tick(window);
        }
    });
}

/// 扫光高光的半宽（按字号算，约两个半字宽）
const SHIMMER_SPREAD_EM: f32 = 2.5;
/// 扫光的颜色从底色到高光分这么多档（每档一条竖向裁切区，档差小到看不出台阶）
const SHIMMER_LEVELS: usize = 10;

/// 新内容入场：淡入的同时从下方这么远的地方升上来
const ENTER_RISE: Pixels = px(8.);

/// 悬停反馈的时长（原版 `--cds-dur-fast`）：比一次性过渡快得多，鼠标划过就该出来
const HOVER_DURATION: Duration = Duration::from_millis(60);

/// 平滑回到底部：固定 320ms 的 cubic-out（比指数衰减更容易感知「到了」），
/// 每帧的目标按实时剩余距离算——流式输出边滑边长高也不会二次跳。
pub(super) const JUMP_DURATION: Duration = Duration::from_millis(320);

/// 这一帧应走掉剩余距离的比例：在固定时长里按 ease-out-cubic 推进，
/// 剩余距离变化（内容还在长）不会改变总时长。
/// `elapsed` 是本帧**终点**时刻（`now - started`），区间是上一帧终点到本帧终点。
pub(super) fn jump_step(elapsed: Duration, dt: Duration) -> f32 {
    let total = JUMP_DURATION.as_secs_f32();
    let prev = elapsed.saturating_sub(dt);
    let q0 = ease_out_cubic((prev.as_secs_f32() / total).min(1.));
    let q1 = ease_out_cubic((elapsed.as_secs_f32() / total).min(1.));
    if q1 >= 1. || 1. - q0 <= f32::EPSILON { 1. } else { ((q1 - q0) / (1. - q0)).clamp(0., 1.) }
}

/// 设置页 / 模型清单的平滑滚动：剩下的距离每过这么久衰减到 1/e。
/// （对话列表不用这套，走 `session::WheelInput`：$C^1$ 三次贝塞尔曲线 + VSync 节拍对齐
/// + 物理像素误差扩散，见那里的注释）
pub(super) const WHEEL_TAU: f32 = 0.05;
/// 一串滚轮最后一格之后最多再滑这么久（正常 4 倍 TAU 就到了，这是兜底）
pub(super) const WHEEL_MAX: Duration = Duration::from_millis(600);
/// 滚轮一格先同步走的比例：手指一滚内容就动（稳重感的来源），剩下部分才进平滑目标
pub(super) const WHEEL_INSTANT: f32 = 0.5;
/// 平滑目标最多领先当前位置这么多：连滚几格时欠账有上限，停下后不会还自己滑一大段
pub(super) const WHEEL_MAX_LEAD: Pixels = px(120.);
/// 设置页一帧最多走这么多：卡顿帧不许一步跨出剩余距离的大半
pub(super) const WHEEL_MAX_STEP: Pixels = px(48.);

/// 「回到底部」的平滑滚动进行中：哪个会话、何时开始、上一帧的时刻。
#[derive(Clone, Copy)]
pub(super) struct JumpAnim {
    pub token: u64,
    pub started: Instant,
    pub last: Instant,
}

/// 「进行中」的双块（当前强调色，12px，界面里所有「进行中」共用这一个：工具行、侧栏、压缩上下文）：
/// 对角两块主色 / 半透明方块，沿四条边轮流伸出去贴满、收回下一个角落；一块 `paint_quad` 一支，
/// 不建路径，圆角在 12px 下也锐利。
/// 减少动态效果时停在静态的对角双块（`phase = None`），不排节拍。
/// 这里只是占位，由 [`LoopOverlay`] 画，只能用在内容区里；别的地方用 [`spinner_in_place`]。
pub(super) fn spinner() -> Div {
    let div = div().size_3().flex_shrink_0();
    if REDUCED.load(Ordering::Relaxed) {
        return div.child(spinner_in_place());
    }
    div.child(loop_spot(LoopKind::Spinner).size_full())
}

/// 就地画的转圈（[`spinner`] 在内容区里只是占位）：给自己就是小覆盖层的 view 用（侧栏转圈）。
pub(super) fn spinner_in_place() -> Div {
    let reduced = REDUCED.load(Ordering::Relaxed);
    let color = rgb(CORAL);
    div().size_3().flex_shrink_0().child(
        canvas(
            move |_, window, _| {
                if !reduced {
                    request_loop_tick(window);
                }
            },
            move |bounds, _, window, _| {
                // 相位取绘制那一刻，而不是 render 时：否则外部重画会把相位钉在旧值上跳一帧
                let phase = (!reduced).then(|| loop_phase(SPIN_PERIOD));
                paint_spinner(bounds, phase, color, window)
            },
        )
        .size_full(),
    )
}

/// 圆周上第 `turns` 圈处的点（0 = 正上方，顺时针）。
pub(super) fn on_circle(center: Point<Pixels>, r: Pixels, turns: f32) -> Point<Pixels> {
    let a = turns * TAU - FRAC_PI_2;
    center + point(r * a.cos(), r * a.sin())
}

/// 实心圆点（圆弧的圆头、星芒的花心）。
pub(super) fn paint_dot(center: Point<Pixels>, r: Pixels, color: Rgba, window: &mut Window) {
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

/// 双块在一圈里的位置：`[x, y, w, h]` 归一化到活动区（单位是活动区边长，`0..1`）。
///
/// 8 个半步一圈：伸出贴满一条边 → 收回角落 → 顺这条边转过 90°。主块顺时针走外圈，
/// 副块取同一条轨迹错开半圈（4 个半步），于是两块永远在对角、都在动，且首尾完全对上
/// （`phase = 0` 与 `1` 是同一组矩形），主副块的透明度不会在接缝处跳。
/// `phase = None`（减少动态效果）停在稳定的对角双块。
fn twin_block_rects(phase: Option<f32>) -> ([f32; 4], [f32; 4]) {
    const U: f32 = TILE_LEN;
    const FAR: f32 = 1. - TILE_LEN;
    // 关键帧 `[x, y, w, h]`（归一化到活动区），依次是：
    // 左上 → 上排满 → 右上 → 右列满 → 右下 → 下排满 → 左下 → 左列满
    const KF: [[f32; 4]; TILE_STEPS] = [
        [0., 0., U, U],
        [0., 0., 1., U],
        [FAR, 0., U, U],
        [FAR, 0., U, 1.],
        [FAR, FAR, U, U],
        [0., FAR, 1., U],
        [0., FAR, U, U],
        [0., 0., U, 1.],
    ];
    let Some(phase) = phase else {
        return (KF[0], KF[TILE_STEPS / 2]);
    };
    let scaled = phase.rem_euclid(1.) * TILE_STEPS as f32;
    let idx = (scaled.floor() as usize) % TILE_STEPS;
    let next = (idx + 1) % TILE_STEPS;
    let local = scaled - scaled.floor();
    // 半步里前面一段位移，后面一段原地停顿：伸展贴合后顿一下再收回，像积木落位
    let t = if local < TILE_MOVE_RATIO { ease_in_out_cubic(local / TILE_MOVE_RATIO) } else { 1. };
    let lerp = |a: [f32; 4], b: [f32; 4]| -> [f32; 4] {
        std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
    };
    let half = TILE_STEPS / 2;
    let main = lerp(KF[idx], KF[next]);
    let secondary = lerp(KF[(idx + half) % TILE_STEPS], KF[(next + half) % TILE_STEPS]);
    (main, secondary)
}

/// 画「进行中」的双块：`bounds` 内接正方形收进一点外边距当活动区，两块圆角方块按
/// [`twin_block_rects`] 摆进去。减少动态效果时 `phase = None`，停在静态对角位。
fn paint_spinner(bounds: Bounds<Pixels>, phase: Option<f32>, color: Rgba, window: &mut Window) {
    let span = bounds.size.width.min(bounds.size.height) * (1. - 2. * TILE_MARGIN);
    // 布局还没量出尺寸的那一帧：没有面积可画，别往场景里塞零尺寸的 quad
    if span <= px(0.) {
        return;
    }
    let origin = bounds.center() - point(span * 0.5, span * 0.5);
    let (main, secondary) = twin_block_rects(phase);
    // 先画压暗的副块再画主块：几何上永不重叠，顺序只是保险
    for (rect, tint) in [(secondary, with_alpha(color, TILE_SECONDARY_ALPHA)), (main, color)] {
        let [x, y, w, h] = rect.map(|v| span * v);
        let area = Bounds::from_corners(origin + point(x, y), origin + point(x + w, y + h));
        let mut quad = fill(area, tint);
        // 圆角按短边算：伸长的条也是同一种圆角，不会被拉成胶囊
        quad.corner_radii = Corners::all(w.min(h) * TILE_RADIUS);
        window.paint_quad(quad);
    }
}

/// 运行状态的几何指示器：常规工作使用统一的双块，需用户处理或特殊状态用线性图标。
pub(super) fn mascot(mood: Mood) -> Div {
    let icon = match mood {
        Mood::Think | Mood::Talk | Mood::Wake | Mood::Scan => {
            return div().size_5().flex_shrink_0().flex().justify_center().items_center().child(
                if REDUCED.load(Ordering::Relaxed) { spinner_in_place() } else { spinner() },
            );
        }
        Mood::Ask => Icon::new(IconAll::CircleAlert).text_color(rgb(CORAL_DEEP)),
        Mood::Sweat => Icon::new(IconAll::TriangleAlert).text_color(rgb(CORAL)),
        Mood::Squish => Icon::new(IconAll::ListCollapse).text_color(rgb(INK_SOFT)),
    };
    div().size_5().flex_shrink_0().flex().justify_center().items_center().child(icon.size_4())
}

/// 扫光文字（「思考中…」、底部进行中状态行）：`base` 色的字上一道 `highlight` 色的柔和高光从左扫到右，约 60 帧。
/// 按像素位置着色：高光强度是余弦钟形，切成 [`SHIMMER_LEVELS`] 档同心竖条，每档用一种颜色
/// 画一遍文字、裁到自己那条（各条不重叠，字形边缘不会被叠画加粗）。排版靠一份透明的同样文字。
/// 减少动态效果时整段都是 `base` 色。
pub(super) fn shimmer_text(text: impl Into<SharedString>, base: Color, highlight: Color) -> Div {
    let text: SharedString = text.into();
    if REDUCED.load(Ordering::Relaxed) {
        return div().text_color(rgb(base)).child(text);
    }
    let (base, highlight) = (rgb(base), rgb(highlight));
    // 排版靠透明的同样文字；字体字号在 prepaint 时取（和这份文字继承同一串祖先的样式）
    let layout = text.clone();
    let spots = CURRENT_SPOTS.with_borrow(Clone::clone);
    div().relative().child(div().text_color(transparent_black()).child(layout)).child(
        canvas(
            move |bounds, window, _| {
                let (style, clip) = (Box::new(window.text_style()), window.content_mask().bounds);
                let kind = LoopKind::Shimmer { text, style, base, highlight };
                if let Some(spots) = &spots {
                    spots.push(LoopSpot { kind, bounds, clip });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0(),
    )
}

#[allow(clippy::too_many_arguments)]
fn paint_shimmer(
    text: &SharedString,
    style: &TextStyle,
    bounds: Bounds<Pixels>,
    phase: f32,
    base: Rgba,
    highlight: Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    // 字体、字号、行高跟占位那份透明文字一样
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

/// 同 [`reveal_progress`]，但 `animate` 为假时直接落位目标值、不排动画帧。
/// 新行涌入、正文到达导致的自动收起用它：0ms 切换，列表高度不抖。
pub(super) fn reveal_progress_when(
    id: impl Into<ElementId>,
    open: bool,
    animate: bool,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let policy = cx.theme().motion_tokens().spring_control.with_travel(animate);
    spring((id.into(), "reveal"), if open { 1f32 } else { 0. }, policy, window, cx)
}

/// 悬停才出现的操作的透明度：60ms 淡入 / 淡出（不挤布局，只改透明度）。
///
/// 悬停反馈要比一次性过渡快得多：原版 `--cds-dur-fast` 就是 60ms，而
/// `motion_tokens().duration_fast` 是 120ms，用在这里显得「肉」——鼠标划过去，
/// 按钮慢半拍才出来。所以这里单开一个常量，不跟主题令牌走。
pub(super) fn hover_opacity(
    id: impl Into<ElementId>,
    hovered: bool,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let policy = Transition::new(HOVER_DURATION);
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
        // 氛围效果强度跟着设置走：描边 / 首页背景在绘制时不一定拿得到设置
        super::prism_rim::set_ambient_level(self.settings.ambient_prism.level());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    /// 两块矩形重叠的面积（归一化单位，贴边不算重叠）
    fn overlap(a: [f32; 4], b: [f32; 4]) -> f32 {
        let w = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
        let h = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
        w.max(0.) * h.max(0.)
    }

    #[core::prelude::v1::test]
    fn twin_blocks_park_on_the_diagonal_when_reduced() {
        const U: f32 = TILE_LEN;
        const FAR: f32 = 1. - TILE_LEN;
        let still = twin_block_rects(None);
        assert!(close(still.0, [0., 0., U, U]) && close(still.1, [FAR, FAR, U, U]), "{still:?}");
        // 静态位就是一圈的起点：放开动画时不会先跳一下
        let start = twin_block_rects(Some(0.));
        assert!(close(start.0, still.0) && close(start.1, still.1), "{start:?}");
    }

    #[core::prelude::v1::test]
    fn twin_blocks_meet_themselves_at_the_loop_seam() {
        let (start, start_secondary) = twin_block_rects(Some(0.));
        let (end, end_secondary) = twin_block_rects(Some(1.));
        assert!(close(start, end) && close(start_secondary, end_secondary), "{start:?} / {end:?}");
        // 相位往回绕、超出 0..1 也要落在同一个位置（帧时间不会累积漂移）
        let wrapped = twin_block_rects(Some(-0.125));
        let forward = twin_block_rects(Some(0.875));
        assert!(close(wrapped.0, forward.0) && close(wrapped.1, forward.1), "{wrapped:?}");
    }

    #[core::prelude::v1::test]
    fn twin_blocks_pause_on_the_next_keyframe() {
        // 半步尾部是原地停顿：已经贴住下一关键帧，不再漂
        let at = |idx: usize, local: f32| {
            twin_block_rects(Some((idx as f32 + local) / TILE_STEPS as f32))
        };
        for idx in 0..TILE_STEPS {
            let paused = at(idx, TILE_MOVE_RATIO + 0.01).0;
            let late = at(idx, 0.99).0;
            let landed = at(idx, 1.).0;
            assert!(close(paused, late), "第 {idx} 步的停顿在漂：{paused:?}");
            assert!(close(paused, landed), "第 {idx} 步没落在关键帧上：{paused:?} / {landed:?}");
        }
    }

    #[core::prelude::v1::test]
    fn twin_blocks_stay_inside_and_apart() {
        // 采样一整圈（含半步交界）：两块都在活动区里，且永不重叠
        for i in 0..=800 {
            let phase = i as f32 / 800.;
            let (main, secondary) = twin_block_rects(Some(phase));
            for rect in [main, secondary] {
                assert!(rect[0] >= -1e-6 && rect[1] >= -1e-6, "越界 {rect:?} @ {phase}");
                assert!(
                    rect[0] + rect[2] <= 1. + 1e-6 && rect[1] + rect[3] <= 1. + 1e-6,
                    "越界 {rect:?} @ {phase}"
                );
            }
            assert!(overlap(main, secondary) <= 0., "两块重叠 @ {phase}");
        }
    }
}
