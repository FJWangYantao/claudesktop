//! 输入框卡片外沿的棱镜描边：SDF 圆角环 + 跟着指针走的径向光谱。
//!
//! 为什么单独一个 view：这是**纯装饰**，每一帧都跟着鼠标重画。如果画在 `ChatApp`
//! 里，鼠标一动整个界面（内容区、侧栏、输入框）都要重排；这里只有它自己被通知，
//! 祖先与兄弟 view 的元素树原样复用。
//!
//! 指针位置放在 `Rc<RefCell<RimPointer>>` 里由根视图写入（见 `ChatApp::track_rim_pointer`）：
//! 根上的 `on_mouse_move` 每一帧都在跑，但**不调 `cx.notify()`**，只有真的值得重画
//! （指针离卡片够近、位移够大、距上次重画过了一帧）时才通知这个 view。
//!
//! 熄灭条件（任何一条成立就一帧都不画，也不排帧）：
//! - 设置里的「氛围效果」是 `Off`；
//! - `cx.reduce_motion()`（设置里的动画关掉或系统减少动态效果）；
//! - 窗口不在前台；
//! - 指针离卡片超过 [`FADE_END`]。
use super::ChatApp;
use crate::theme::{self, COMPOSER_RADIUS};
use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// 指针进到这个距离以内，描边就已经是满亮。
const FADE_START: f32 = 24.;
/// 指针超过这个距离就完全熄灭（连重绘都不排）。
const FADE_END: f32 = 220.;
/// 光谱半径：指针离边框这么近时，绕一圈的色相铺满。
const SPECTRUM_RADIUS: f32 = 200.;
/// 两次重绘之间至少隔这么久（约 60Hz），鼠标回报率再高也不刷爆。
const MIN_FRAME_GAP: Duration = Duration::from_millis(16);
/// 指针位移小于这个值就当作没动。
const MIN_MOVE: f32 = 1.5;

/// 根视图与描边 view 共享的指针状态。
#[derive(Default)]
pub(crate) struct RimPointer {
    /// 指针最近一次的位置（窗口坐标，逻辑像素）
    position: Option<Point<Pixels>>,
    /// 上一帧真的画出去的位置（位移阈值比较用）
    painted: Option<Point<Pixels>>,
    /// 上一次通知重绘的时刻
    last_notify: Option<Instant>,
}

/// 输入框外沿的棱镜描边。
pub(crate) struct ComposerRimView {
    pointer: Rc<RefCell<RimPointer>>,
    /// 卡片这一帧的窗口坐标（prepaint 时写入）。根视图用它做「指针离得远就不重绘」
    /// 的距离剪枝——这是真正 0fps 的关键：只在需要画的时候才通知。
    card_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl ComposerRimView {
    pub(super) fn new(pointer: Rc<RefCell<RimPointer>>) -> Self {
        Self {
            pointer,
            card_bounds: Rc::new(Cell::new(None)),
        }
    }

    /// 卡片上一帧的窗口坐标（还没画过时为 `None`）。
    pub(super) fn card_bounds(&self) -> Option<Bounds<Pixels>> {
        self.card_bounds.get()
    }
}

impl Render for ComposerRimView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pointer = self.pointer.clone();
        let card_bounds = self.card_bounds.clone();
        // 这几项每帧读一次全局状态：配色 / 深浅 / 减少动态都直接影响画不画。
        let ambient = if cx.reduce_motion() { 0. } else { 1. };
        let dark = theme::is_dark();
        let tone = theme::prism_tone();

        canvas(
            move |bounds, _, _| {
                // 记下这一帧的卡片位置：根视图的鼠标回调靠它判断要不要通知我们。
                card_bounds.set(Some(bounds));
            },
            move |bounds, _, window, _| {
                if ambient <= 0.001 {
                    return;
                }
                // 窗口不在前台时完全不画：切去别的程序后输入框回到静止样子
                // （和 `composer_focus_shadow` 的聚焦判定一致）。
                if !window.is_window_active() {
                    return;
                }
                let pointer = pointer.borrow().position;
                let Some(position) = pointer else { return };
                let alpha = proximity_alpha(distance_to_bounds(bounds, position));
                if alpha <= 0.002 {
                    return;
                }
                let rem = window.rem_size();
                // 描边铺在卡片外沿 1px 上：画布比卡片各边多 1px，圆角跟着多 1px 保持同心。
                let radii = COMPOSER_RADIUS.to_pixels(rem) + px(1.);
                window.paint_prism_rim(
                    bounds,
                    PrismRimStyle {
                        corner_radii: Corners::all(radii),
                        pointer: position,
                        radius: px(SPECTRUM_RADIUS),
                        stroke_width: px(1.),
                        alpha: alpha * ambient * ambient_level(),
                        dark,
                        tone_key: tone,
                        mode: 0,
                    },
                );
            },
        )
        // 四边各外扩 1px：圆角环骑在卡片边上，里外各占一半，太窄会被自己的
        // quad 裁掉、看起来像贴着内侧的一条线。
        .absolute()
        .top(px(-1.))
        .left(px(-1.))
        .right(px(-1.))
        .bottom(px(-1.))
    }
}

impl ChatApp {
    /// 根视图的 `on_mouse_move` 调这个：记住指针位置，必要时才通知描边重绘。
    ///
    /// 注意这里**不**调 `cx.notify()`：通知的是 `rim_view` 自己，`ChatApp` 与内容区
    /// 的元素树复用上一帧。
    pub(super) fn track_rim_pointer(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if cx.reduce_motion() || !window.is_window_active() || ambient_level() <= 0.001 {
            return;
        }
        let Some(bounds) = self.rim_view.read(cx).card_bounds() else {
            return;
        };
        if distance_to_bounds(bounds, position) > FADE_END {
            return;
        }

        let now = Instant::now();
        let notify = {
            let mut pointer = self.rim_pointer.borrow_mut();
            pointer.position = Some(position);
            let too_soon = pointer
                .last_notify
                .is_some_and(|last| now.saturating_duration_since(last) < MIN_FRAME_GAP);
            let painted = pointer.painted.unwrap_or(position);
            let moved = (f32::from(position.x) - f32::from(painted.x)).abs() >= MIN_MOVE
                || (f32::from(position.y) - f32::from(painted.y)).abs() >= MIN_MOVE;
            if too_soon || !moved {
                false
            } else {
                pointer.last_notify = Some(now);
                pointer.painted = Some(position);
                true
            }
        };
        if notify {
            self.rim_view.update(cx, |_, cx| cx.notify());
        }
    }
}

/// 「氛围效果」强度；`Off` 时描边一帧都不画。
fn ambient_level() -> f32 {
    AMBIENT_LEVEL.with(|level| level.get())
}

thread_local! {
    /// 当前氛围强度（`ChatApp::apply_motion_pref` 时同步，绘制时不一定拿得到设置）。
    static AMBIENT_LEVEL: Cell<f32> = const { Cell::new(1.) };
}

/// 设置变更时同步氛围强度（由 `ChatApp` 在换设置 / 换配色时调用）。
pub(super) fn set_ambient_level(level: f32) {
    AMBIENT_LEVEL.with(|cell| cell.set(level.clamp(0., 1.)));
}

/// 指针到矩形外沿的距离（在矩形内为 0）。
fn distance_to_bounds(bounds: Bounds<Pixels>, position: Point<Pixels>) -> f32 {
    let (left, top) = (f32::from(bounds.left()), f32::from(bounds.top()));
    let (right, bottom) = (f32::from(bounds.right()), f32::from(bounds.bottom()));
    let (x, y) = (f32::from(position.x), f32::from(position.y));
    let dx = (left - x).max(x - right).max(0.);
    let dy = (top - y).max(y - bottom).max(0.);
    (dx * dx + dy * dy).sqrt()
}

/// 距离 → 不透明度：近处满亮，远处平滑熄灭。
fn proximity_alpha(distance: f32) -> f32 {
    if distance >= FADE_END {
        return 0.;
    }
    if distance <= FADE_START {
        return 1.;
    }
    let t = (FADE_END - distance) / (FADE_END - FADE_START);
    t * t * (3. - 2. * t)
}
