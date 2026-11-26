//! claudesktop patch: 输入框光标滑行。
//!
//! 光标换位置时（打字、方向键、跨行、点选）不再瞬移，用 55ms 从旧位置滑到新位置，
//! 曲线取 CSS 的 `cubic-bezier(0.22, 1, 0.36, 1)`（快起、长尾，末尾几乎停住）。
//!
//! 三类情况必须**直接落位**，滑行会变成穿帮的拖尾：
//!
//! - 刚聚焦（`!was_focused`）：上一次的位置还在别的输入框里；
//! - 文档被清空（发送后）：光标从末尾弹回开头，滑过去像找不到家；
//! - 光标尺寸变了（字号 / 行高 / 换行变了）。
//!
//! 另外「纯滚动」要整体平移 `from`/`to`：滚动时光标在屏幕上移动不是因为它换了字符位置，
//! 补偿掉这段位移才不会在滚动时看到光标慢慢追上来。
//!
//! 是否真的该滑（单光标、无选区、`App::reduce_motion` 为假、窗口在前台）由调用方
//! [`crate::input::element::TextElement`] 判断后传 `animate`，这里只管算位置。
use gpui::{Bounds, Pixels, Point, point, px, size};
use std::time::{Duration, Instant};

/// 一程滑行走完的时间。
pub(crate) const DURATION: Duration = Duration::from_millis(55);

// cubic-bezier(0.22, 1.0, 0.36, 1.0) 的两个控制点。
const CONTROL_X1: f32 = 0.22;
const CONTROL_Y1: f32 = 1.0;
const CONTROL_X2: f32 = 0.36;
const CONTROL_Y2: f32 = 1.0;

/// 这一帧要画的光标。
#[derive(Clone, Copy, Debug)]
pub(crate) struct SmoothCaretFrame {
    pub bounds: Bounds<Pixels>,
    /// 还在滑：调用方要为下一帧 `Window::request_animation_frame`。
    pub animating: bool,
}

impl SmoothCaretFrame {
    fn settled(bounds: Bounds<Pixels>) -> Self {
        Self { bounds, animating: false }
    }
}

/// 光标滑行的状态，跟着输入框状态走（每个输入框一份）。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SmoothCaretState {
    from: Bounds<Pixels>,
    to: Bounds<Pixels>,
    started: Option<Instant>,
    /// 上一帧光标吃到的滚动偏移（x 已折算进 `bounds`，y 由 `cursor_infos_with_scroll` 补）。
    scroll: Point<Pixels>,
    /// 上一帧是否聚焦：`false -> true` 的那一帧直接落位。
    focused: bool,
    /// 上一帧文档是否为空：非空变空的那一帧直接落位。
    empty: bool,
    /// `from`/`to` 有没有被写过（第一次画光标不能从 (0,0) 滑过来）。
    primed: bool,
}

impl SmoothCaretState {
    /// 这一帧要画的位置。
    ///
    /// - `target`：布局算出来的光标位置（屏幕坐标，含滚动）；
    /// - `scroll`：这一帧光标对应的滚动偏移，只用来算补偿；
    /// - `focused`：输入框这一帧是否聚焦；
    /// - `empty`：文档这一帧是否为空；
    /// - `animate`：调用方算好的「这一帧该不该滑」（`false` 直接落位）；
    /// - `scale_factor`：屏幕缩放，滑行路过的位置对齐到设备像素，免得 0.5px 发虚。
    pub(crate) fn advance(
        &mut self,
        target: Bounds<Pixels>,
        scroll: Point<Pixels>,
        focused: bool,
        empty: bool,
        animate: bool,
        scale_factor: f32,
    ) -> SmoothCaretFrame {
        self.advance_at(Instant::now(), target, scroll, focused, empty, animate, scale_factor)
    }

    /// [`Self::advance`] 的实现，时间点由调用方给（单测用）。
    #[allow(clippy::too_many_arguments)]
    fn advance_at(
        &mut self,
        now: Instant,
        target: Bounds<Pixels>,
        scroll: Point<Pixels>,
        focused: bool,
        empty: bool,
        animate: bool,
        scale_factor: f32,
    ) -> SmoothCaretFrame {
        let snap = !animate
            || !self.primed
            || !self.focused
            || target.size != self.to.size
            || (!self.empty && empty);

        let scroll_delta = scroll - self.scroll;
        self.scroll = scroll;
        self.focused = focused;
        self.empty = empty;

        if snap {
            self.from = target;
            self.to = target;
            self.started = None;
            self.primed = true;
            return SmoothCaretFrame::settled(target);
        }

        // 纯滚动：起点终点一起挪，滚动不该产生「追上来」的拖尾。
        if scroll_delta != Point::default() {
            self.from.origin = self.from.origin + scroll_delta;
            self.to.origin = self.to.origin + scroll_delta;
        }

        if target != self.to {
            // 目标换了（打字 / 方向键 / 点选）：从眼下插值到的位置重新起步。
            self.from = self.pose(now);
            self.to = target;
            self.started = Some(now);
        }

        let Some(started) = self.started else {
            return SmoothCaretFrame::settled(self.to);
        };
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= DURATION {
            self.started = None;
            return SmoothCaretFrame::settled(self.to);
        }

        let progress = cubic_bezier_ease(elapsed.as_secs_f32() / DURATION.as_secs_f32());
        SmoothCaretFrame {
            bounds: snap_to_device_pixels(lerp_bounds(self.from, self.to, progress), scale_factor),
            animating: true,
        }
    }

    /// 此刻已经滑到哪儿了（中途换目标要用它当新起点，否则会跳一下）。
    fn pose(&self, now: Instant) -> Bounds<Pixels> {
        let Some(started) = self.started else {
            return self.to;
        };
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= DURATION {
            return self.to;
        }
        let progress = cubic_bezier_ease(elapsed.as_secs_f32() / DURATION.as_secs_f32());
        lerp_bounds(self.from, self.to, progress)
    }
}

fn lerp_bounds(from: Bounds<Pixels>, to: Bounds<Pixels>, progress: f32) -> Bounds<Pixels> {
    let progress = progress.clamp(0., 1.);
    let origin = point(
        from.origin.x + (to.origin.x - from.origin.x) * progress,
        from.origin.y + (to.origin.y - from.origin.y) * progress,
    );
    // 尺寸不插值：行高一变就该直接换，别看出光标在长高。
    Bounds::new(origin, to.size)
}

/// 让画出来的位置落在设备像素上：小数点后的偏移会让 1~2px 宽的光标发虚。
fn snap_to_device_pixels(bounds: Bounds<Pixels>, scale_factor: f32) -> Bounds<Pixels> {
    if !(scale_factor.is_finite() && scale_factor > 0.) {
        return bounds;
    }
    let snap = |value: Pixels| px((f32::from(value) * scale_factor).round() / scale_factor);
    Bounds::new(
        point(snap(bounds.origin.x), snap(bounds.origin.y)),
        size(snap(bounds.size.width), snap(bounds.size.height)),
    )
}

/// `cubic-bezier(0.22, 1, 0.36, 1)`：给定时间进度（0~1）返回位移进度。
///
/// CSS 的曲线是参数方程，横轴也是三次的，所以要先反解 `x(t) = u` 再取 `y(t)`。
/// 用二分而不上牛顿：这条曲线在两端斜率很大，牛顿容易过冲，二分八步的精度（1/256）
/// 在 55ms、几十像素的位移上小于 0.2px，够用。
fn cubic_bezier_ease(progress: f32) -> f32 {
    let u = progress.clamp(0., 1.);
    if u <= 0. {
        return 0.;
    }
    if u >= 1. {
        return 1.;
    }

    fn bezier(t: f32, p1: f32, p2: f32) -> f32 {
        let u = 1. - t;
        3. * u * u * t * p1 + 3. * u * t * t * p2 + t * t * t
    }

    let mut low = 0.;
    let mut high = 1.;
    let mut t = u;
    for _ in 0..8 {
        let x = bezier(t, CONTROL_X1, CONTROL_X2);
        if (x - u).abs() < 1e-4 {
            break;
        }
        if x < u {
            low = t;
        } else {
            high = t;
        }
        t = 0.5 * (low + high);
    }
    bezier(t, CONTROL_Y1, CONTROL_Y2).clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(x: f32) -> Bounds<Pixels> {
        Bounds::new(point(gpui::px(x), gpui::px(10.)), size(gpui::px(2.), gpui::px(16.)))
    }

    fn at(millis: u64) -> Instant {
        // 用固定基准 + 偏移，绕开 `Instant::now()` 不可控的问题。
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_millis(millis)
    }

    #[test]
    fn ease_hits_both_ends_and_stays_monotonic() {
        assert_eq!(cubic_bezier_ease(0.), 0.);
        assert_eq!(cubic_bezier_ease(1.), 1.);
        let mut last = 0.;
        for step in 0..=20 {
            let progress = step as f32 / 20.;
            let eased = cubic_bezier_ease(progress);
            assert!(eased >= last, "{progress} 处回退了：{eased} < {last}");
            last = eased;
        }
        // 快起：走过一半时间时位移已经过半。
        assert!(cubic_bezier_ease(0.5) > 0.5);
    }

    #[test]
    fn first_frame_lands_without_sliding() {
        let mut state = SmoothCaretState::default();
        let frame = state.advance_at(at(0), caret(40.), Point::default(), true, false, true, 2.);
        assert_eq!(frame.bounds.origin.x, gpui::px(40.));
        assert!(!frame.animating);
    }

    #[test]
    fn moved_caret_slides_then_settles() {
        let mut state = SmoothCaretState::default();
        state.advance_at(at(0), caret(40.), Point::default(), true, false, true, 1.);
        // 目标换成右边 60px，起步这一帧仍在旧位置。
        let start = state.advance_at(at(100), caret(100.), Point::default(), true, false, true, 1.);
        assert_eq!(start.bounds.origin.x, gpui::px(40.));
        assert!(start.animating);

        let middle =
            state.advance_at(at(127), caret(100.), Point::default(), true, false, true, 1.);
        let x = f32::from(middle.bounds.origin.x);
        assert!((40.0..100.0).contains(&x), "中途应在起终点之间：{x}");
        assert!(middle.animating);

        let end = state.advance_at(at(200), caret(100.), Point::default(), true, false, true, 1.);
        assert_eq!(end.bounds.origin.x, gpui::px(100.));
        assert!(!end.animating, "走完这一程就不该再排帧");
    }

    #[test]
    fn retargeting_mid_flight_continues_from_where_it_is() {
        let mut state = SmoothCaretState::default();
        state.advance_at(at(0), caret(0.), Point::default(), true, false, true, 1.);
        state.advance_at(at(10), caret(100.), Point::default(), true, false, true, 1.);
        let mid = state.advance_at(at(30), caret(100.), Point::default(), true, false, true, 1.);
        let mid_x = f32::from(mid.bounds.origin.x);
        // 半路把目标换到左边：这一帧应当还停在原处（从脚下重新起步），不跳回起点也不瞬到终点。
        let switched = state.advance_at(at(31), caret(0.), Point::default(), true, false, true, 1.);
        assert!(switched.animating);
        assert!(
            (f32::from(switched.bounds.origin.x) - mid_x).abs() < 5.,
            "换目标那一帧跳了：{mid_x} → {}",
            f32::from(switched.bounds.origin.x)
        );
    }

    #[test]
    fn clearing_the_document_lands_instantly() {
        let mut state = SmoothCaretState::default();
        state.advance_at(at(0), caret(200.), Point::default(), true, false, true, 1.);
        let frame = state.advance_at(at(10), caret(0.), Point::default(), true, true, true, 1.);
        assert_eq!(frame.bounds.origin.x, gpui::px(0.));
        assert!(!frame.animating, "清空后不该从末尾慢慢滑回开头");
    }

    #[test]
    fn refocus_lands_instantly() {
        let mut state = SmoothCaretState::default();
        state.advance_at(at(0), caret(200.), Point::default(), true, false, true, 1.);
        let frame = state.advance_at(at(10), caret(0.), Point::default(), false, false, true, 1.);
        assert_eq!(frame.bounds.origin.x, gpui::px(0.));
        let frame = state.advance_at(at(20), caret(0.), Point::default(), true, false, true, 1.);
        assert!(!frame.animating);
    }

    #[test]
    fn scrolling_shifts_both_ends_so_the_caret_does_not_lag() {
        let mut state = SmoothCaretState::default();
        state.advance_at(at(0), caret(40.), Point::default(), true, false, true, 1.);
        // 横向滚 20px：光标位置跟着从 40 变成 20，这不算「换了目标」，所以不滑。
        let frame = state.advance_at(
            at(10),
            caret(20.),
            point(gpui::px(-20.), gpui::px(0.)),
            true,
            false,
            true,
            1.,
        );
        assert_eq!(frame.bounds.origin.x, gpui::px(20.));
        assert!(!frame.animating);
    }

    #[test]
    fn reduce_motion_never_starts_an_animation() {
        let mut state = SmoothCaretState::default();
        state.advance_at(at(0), caret(40.), Point::default(), true, false, true, 1.);
        let frame = state.advance_at(at(10), caret(100.), Point::default(), true, false, false, 1.);
        assert_eq!(frame.bounds.origin.x, gpui::px(100.));
        assert!(!frame.animating);
    }

    #[test]
    fn device_pixel_snapping_keeps_the_caret_sharp() {
        let mut state = SmoothCaretState::default();
        state.advance_at(at(0), caret(40.), Point::default(), true, false, true, 1.5);
        let frame = state.advance_at(at(30), caret(100.), Point::default(), true, false, true, 1.5);
        let scaled = f32::from(frame.bounds.origin.x) * 1.5;
        assert!((scaled - scaled.round()).abs() < 1e-3, "1.5 倍缩放下应落在设备像素上：{scaled}");
    }
}
