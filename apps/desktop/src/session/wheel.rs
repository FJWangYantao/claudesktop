//! 对话滚轮的平滑滚动：欠量按 $C^1$ 三次贝塞尔曲线逐帧推进，按物理像素取整 + 误差扩散交给列表。

use gpui_kit::*;
use std::time::{Duration, Instant};

/// Chromium `INVERSE_DELTA`：欠量从 120px 增至 480px 时，段长从 200ms 缩至 100ms。
const WHEEL_DUR_MAX: Duration = Duration::from_millis(200);
const WHEEL_DUR_MIN: Duration = Duration::from_millis(100);
const WHEEL_DUR_RAMP_START: f32 = 120.;
const WHEEL_DUR_RAMP_SPAN: f32 = 360.;
/// `EASE_IN_OUT` 固定横坐标，连滚仅调整起手斜率。
const WHEEL_X1: f32 = 0.42;
const WHEEL_X2: f32 = 0.58;
/// 按继承的速度缩短段长，使第一个控制点的 y 不超过 1。
const WHEEL_VELOCITY_BOUND: f32 = 1. / WHEEL_X1;
/// 高刷不必反复截断高速连滚，180Hz 时单周期最多推进约 33px。
const WHEEL_MAX_SPEED: f32 = 6000.;
/// 一帧最多走的像素：卡顿帧（进了高行要排版）不许一步跨出去，宁可多滑几帧
const WHEEL_STEP_MAX: Pixels = px(48.);
/// 余量一直走不出去（列表还没量出那段，或正好在边界）最多再等这么久；
/// 超过就丢掉，不会一直排帧。
const WHEEL_HOLD: Duration = Duration::from_millis(400);
/// 两帧之间隔了这么久说明列表压根没在画（去了设置页 / 最小化）：这期间的余量全部作废，
/// 回来时不能把几秒前的滚轮一次性补上。
const WHEEL_STALE: Duration = Duration::from_millis(500);
/// 平台查不到显示器刷新率时的兜底帧间隔（60Hz）
pub const DEFAULT_FRAME_DT: Duration = Duration::from_micros(16_667);

/// 求解三次贝塞尔曲线 (0,0)-(x1,y1)-(x2,1)-(1,1) 在横坐标 `x ∈ [0,1]` 处的纵坐标和斜率 dy/dx。
/// 牛顿迭代 + 二分兜底：x1, x2 ∈ [0,1] 且 y1 ∈ [0,1] 时曲线严格单调、零过冲。
fn cubic_bezier_y_and_slope(x: f32, x1: f32, y1: f32, x2: f32) -> (f32, f32) {
    if x <= 0. {
        return (0., if x1 > 1e-6 { y1 / x1 } else { 0. });
    }
    if x >= 1. {
        return (1., 0.);
    }
    let mut t = x;
    let (mut lo, mut hi) = (0f32, 1f32);
    for _ in 0..10 {
        let mt = 1. - t;
        let bx = 3. * mt * mt * t * x1 + 3. * mt * t * t * x2 + t * t * t;
        let err = bx - x;
        if err.abs() < 1e-6 {
            break;
        }
        if err < 0. {
            lo = t;
        } else {
            hi = t;
        }
        let dbx = 3. * mt * mt * x1 + 6. * mt * t * (x2 - x1) + 3. * t * t * (1. - x2);
        if dbx > 1e-6 {
            let next = t - err / dbx;
            t = if next > lo && next < hi { next } else { 0.5 * (lo + hi) };
        } else {
            t = 0.5 * (lo + hi);
        }
    }
    let mt = 1. - t;
    let by = 3. * mt * mt * t * y1 + 3. * mt * t * t + t * t * t;
    let dbx = 3. * mt * mt * x1 + 6. * mt * t * (x2 - x1) + 3. * t * t * (1. - x2);
    let dby = 3. * mt * mt * y1 + 6. * mt * t * (1. - y1);
    let slope = if dbx > 1e-6 { (dby / dbx).max(0.) } else { 0. };
    (by, slope)
}

/// `advance` 算出、等 `consume` 拿到实际位移之后再提交（或按实际速度重锚）的一帧计划。
#[derive(Clone, Copy, Debug)]
struct PendingStep {
    next_elapsed: Duration,
    next_pos: f32,
    next_vel: f32,
    next_err: f32,
    capped: bool,
    planned_abs: f32,
}

/// 对话的滚轮平滑状态：只存用户还要滚多少像素（`remaining`），每帧沿一段
/// $C^1$ 连续的三次贝塞尔曲线推进，并按物理像素取整 + 误差扩散交给列表。
///
/// 和旧写法（记住滚动条的绝对目标、每帧用读数逼近）的区别在于这里**完全不依赖滚动条读数**：
/// 上方行补量会抬高滚动条读数，但欠量只由输入和已走的距离决定，补量不会凭空造出位移；
/// 同向连滚把当前速度接成新段的起手斜率（速度连续、不冲不顿）；反向输入清空旧余量重新起步。
#[derive(Clone, Debug, Default)]
pub struct WheelInput {
    /// 用户还要滚多少像素（正 = 向下，负 = 向上）
    remaining: Pixels,
    /// 当前瞬时速度（带符号，px/s）：同向追加输入时继承为新段起手斜率
    vel: f32,
    /// 当前曲线段的目标总长（无符号）与已经走到的曲线位置（无符号）
    seg_dist: f32,
    seg_pos: f32,
    seg_elapsed: Duration,
    seg_dur: Duration,
    /// 当前曲线段的第一个控制点 (`WHEEL_X1`, y1)。
    y1: f32,
    /// 物理像素取整后的小数余量（无符号，沿当前方向累加到下一帧，总位移严格守恒）
    pixel_err: f32,
    /// 上一次采样（或新串 push）的时刻
    last: Option<Instant>,
    last_was_vsync: bool,
    /// 本串是否已经推进过首帧：首帧固定按一个刷新周期起步，消除事件到达相位的抖动
    started_frame: bool,
    /// 还欠着却一步都走不出去是从什么时候开始的（超过 `WHEEL_HOLD` 就认定到边界）
    stalled_at: Option<Instant>,
    /// `advance` 算出的待确认状态，`consume` 收到实际位移后提交或重锚
    pending: Option<PendingStep>,
}

impl WheelInput {
    pub fn new(now: Instant, delta: Pixels) -> Self {
        let mut w = Self::default();
        w.push(now, delta);
        w
    }

    /// 欠量越大，段时长越短。连滚继承当前速度，必要时缩短时长来限制起手斜率。
    fn plan_segment(&mut self, vel_abs: f32) {
        let d = self.remaining.as_f32().abs();
        self.seg_dist = d;
        self.seg_pos = 0.;
        self.seg_elapsed = Duration::ZERO;
        let ratio = ((d - WHEEL_DUR_RAMP_START) / WHEEL_DUR_RAMP_SPAN).clamp(0., 1.);
        let max_s = WHEEL_DUR_MAX.as_secs_f32();
        let min_s = WHEEL_DUR_MIN.as_secs_f32();
        let mut dur_s = max_s - (max_s - min_s) * ratio;
        if vel_abs <= 1e-3 || d <= 1e-4 {
            self.y1 = 0.;
        } else {
            let bound_s = (d / vel_abs) * WHEEL_VELOCITY_BOUND;
            dur_s = dur_s.min(bound_s).max(0.016);
            let slope = vel_abs * dur_s / d;
            self.y1 = (slope * WHEEL_X1).min(1.);
        }
        self.seg_dur = Duration::from_secs_f32(dur_s);
    }

    /// 追加一次输入。方向和已有的一致就累进剩余位移并接上当前速度，反了（或已走完 / 隔太久）就整条重来。
    pub fn push(&mut self, now: Instant, delta: Pixels) {
        if delta == px(0.) {
            return;
        }
        let down = delta > px(0.);
        // 距上一次采样太久（最小化后恢复）：那串已经不成立了，整条重来，
        // 免得 `advance` 的过期判定把刚推入的这一格也一起清掉
        let stale =
            self.last.is_some_and(|last| now.saturating_duration_since(last) >= WHEEL_STALE);
        let fresh = self.remaining.as_f32().abs() <= 1e-4;
        let old_down = self.remaining > px(0.);
        if fresh || stale || old_down != down {
            // 新的一串 / 换了方向 / 隔了太久：旧方向的余量作废，从此刻起算
            self.remaining = delta;
            self.vel = 0.;
            self.pixel_err = 0.;
            self.last = Some(now);
            self.last_was_vsync = false;
            self.started_frame = false;
            self.plan_segment(0.);
        } else {
            // 同向连滚：保留当前速度做 $C^1$ 衔接，不打断首帧节拍
            self.remaining += delta;
            self.plan_segment(self.vel.abs());
        }
        // 新的输入重新给足等待窗口：卡顿期同向再滚一格不该沿用旧的计时
        self.stalled_at = None;
        self.pending = None;
    }

    /// 推进一帧：`frame_dt` 是显示器刷新周期，`scale` 是窗口 DPI 缩放比。
    /// 采样时间和当前时间都从外面传，测试可以注入固定时刻。
    ///
    /// 两帧间隔太久（去了设置页、窗口最小化）时作废：隔了几秒回来不能把之前的滚轮补上。
    pub fn advance(
        &mut self,
        now: Instant,
        vsync_at: Option<Instant>,
        frame_dt: Duration,
        scale: f32,
    ) -> Pixels {
        self.pending = None;
        let rem = self.remaining.as_f32();
        // 已经不到 0.0001px：当作走完（顺手清掉，别让极小残留让 `is_done` 永远为假、每帧空排）
        if rem.abs() <= 1e-4 {
            self.clear();
            return px(0.);
        }
        let frame_dt = if frame_dt.is_zero() { DEFAULT_FRAME_DT } else { frame_dt };
        let scale = if scale > 0. { scale } else { 1. };

        // 首帧固定走一个刷新周期，随后优先按合成器的节拍推进。
        let sample = vsync_at.unwrap_or(now);
        let paced_dt = if !self.started_frame {
            if self.last.is_some_and(|last| now.saturating_duration_since(last) >= WHEEL_STALE) {
                self.clear();
                return px(0.);
            }
            self.started_frame = true;
            frame_dt
        } else {
            let last = self.last.unwrap_or(sample);
            let elapsed = sample.saturating_duration_since(last);
            if elapsed >= WHEEL_STALE {
                self.clear();
                return px(0.);
            }
            let min_gap = if vsync_at.is_none() && self.last_was_vsync {
                // 原生帧之后的同周期杂重绘不重复推进。
                frame_dt.mul_f32(0.85)
            } else {
                frame_dt.mul_f32(0.25).min(Duration::from_millis(2))
            };
            if elapsed < min_gap {
                return px(0.);
            }
            let ratio = elapsed.as_secs_f32() / frame_dt.as_secs_f32();
            let frames = if vsync_at.is_some() { ratio.round() } else { (ratio + 0.25).floor() };
            frame_dt.mul_f32(frames.clamp(1., 4.))
        };
        self.last = Some(sample);
        self.last_was_vsync = vsync_at.is_some();

        let sign = rem.signum();
        let rem_abs = rem.abs();
        let dt_s = paced_dt.as_secs_f32();
        let next_elapsed = self.seg_elapsed + paced_dt;
        let dur_s = self.seg_dur.as_secs_f32().max(1e-4);
        let u = (next_elapsed.as_secs_f32() / dur_s).min(1.);
        // 限速按物理像素算，高刷下也不至于一帧走不动
        let max_step = (WHEEL_MAX_SPEED * dt_s).max(1. / scale).min(WHEEL_STEP_MAX.as_f32());

        let (mut raw, next_pos, mut next_vel) = if u >= 1. {
            (rem_abs, self.seg_dist, 0.)
        } else {
            let (by, slope) = cubic_bezier_y_and_slope(u, WHEEL_X1, self.y1, WHEEL_X2);
            let pos = self.seg_dist * by;
            let step = (pos - self.seg_pos).clamp(0., rem_abs);
            (step, pos, (self.seg_dist / dur_s) * slope)
        };

        let mut capped = false;
        if raw > max_step {
            raw = max_step;
            next_vel = max_step / dt_s;
            capped = true;
        }

        // 物理像素取整 + 误差扩散：每帧按整数物理像素平移，总位移不丢亚像素
        let (step_abs, next_err) = if u >= 1. && !capped {
            (rem_abs, 0.)
        } else if capped {
            // 限速帧不叠旧误差：向下取整到物理像素（至少一个），严格不越过 `max_step`。
            // 下一帧 `consume` 会按实际速度重锚，误差本来也不该跨过限速边界积累
            let q = ((raw * scale).floor() / scale).max(1. / scale).min(rem_abs);
            (q, 0.)
        } else {
            let want = raw + self.pixel_err;
            let q = ((want * scale).round() / scale).clamp(0., rem_abs);
            // 尾段只剩不到半个物理像素时当帧顺手收干净
            if rem_abs - q < 0.5 / scale && u >= 0.9 { (rem_abs, 0.) } else { (q, want - q) }
        };

        // 高刷尾段单帧不足半个物理像素：返回 0 直接记账，等攒够一个物理像素再挪
        if step_abs == 0. {
            self.seg_elapsed = next_elapsed;
            self.seg_pos = next_pos;
            self.vel = sign * next_vel;
            self.pixel_err = next_err;
            return px(0.);
        }

        self.pending = Some(PendingStep {
            next_elapsed,
            next_pos,
            next_vel: sign * next_vel,
            next_err,
            capped,
            planned_abs: step_abs,
        });
        px(sign * step_abs)
    }

    /// 记账：实际走了 `applied`（边界裁剪之后）。只有真走动了才提交这一帧的曲线进度——
    /// 被卡住（等未测行 / 撞边界）时重锚并开始计「卡住」的时长，否则解堵那一帧会把
    /// 剩余距离一次性吐出来。
    pub fn consume(&mut self, applied: Pixels, now: Instant) {
        self.remaining -= applied;
        if self.remaining.as_f32().abs() <= 1e-4 {
            self.clear();
            return;
        }
        let pend = self.pending.take();
        if applied == px(0.) {
            // 计划了却没走动（等未测行 / 撞边界）：曲线时间不推进，重锚并开始计「卡住」。
            // 没有计划（高刷尾段不到半个物理像素，`advance` 已自己记过账）就什么都不做。
            if pend.is_none() {
                return;
            }
            self.vel = 0.;
            self.pixel_err = 0.;
            self.stalled_at.get_or_insert(now);
            self.plan_segment(0.);
            return;
        }
        self.stalled_at = None;
        if let Some(p) = pend {
            let applied_abs = applied.as_f32().abs();
            if p.capped || (applied_abs - p.planned_abs).abs() > 1e-3 {
                // 被限速或部分截断：按当前实际速度重锚剩余距离，平滑脱离限速
                self.vel = p.next_vel;
                self.pixel_err = 0.;
                self.plan_segment(p.next_vel.abs());
            } else {
                self.seg_elapsed = p.next_elapsed;
                self.seg_pos = p.next_pos;
                self.vel = p.next_vel;
                self.pixel_err = p.next_err;
            }
        }
    }

    /// 全部作废（切页、定位、列表重置、太长时间没画）。
    pub fn clear(&mut self) {
        self.remaining = px(0.);
        self.vel = 0.;
        self.seg_dist = 0.;
        self.seg_pos = 0.;
        self.seg_elapsed = Duration::ZERO;
        self.last = None;
        self.last_was_vsync = false;
        self.started_frame = false;
        self.pixel_err = 0.;
        self.stalled_at = None;
        self.pending = None;
    }

    /// 这个方向已经到头：朝这个方向的余量作废，别每帧空转。
    pub fn block(&mut self, down: bool) {
        if self.remaining != px(0.) && (self.remaining > px(0.)) == down {
            self.clear();
        }
    }

    /// 欠量一直走不出去太久了（列表放不出这段距离）：认定到边界，丢掉余量。
    pub fn expired(&self, now: Instant) -> bool {
        self.stalled_at.is_some_and(|at| now.saturating_duration_since(at) >= WHEEL_HOLD)
    }

    /// 欠量走完了（或卡住超时）：不再排帧。阈值和 `advance` / `consume` 保持一致，
    /// 否则极小残留会让 `advance` 早退、`is_done` 又不认，每帧空排。
    pub fn is_done(&self, now: Instant) -> bool {
        self.remaining.as_f32().abs() <= 1e-4 || self.expired(now)
    }

    /// 还有多少欠量（只在测试里看）。
    #[cfg(test)]
    pub fn remaining(&self) -> Pixels {
        self.remaining
    }
}

/// 设置页 / 模型清单的平滑滚动：滚轮只推进 `target`（离列表顶部的像素），实际位置每帧往它逼近一步。
#[derive(Clone, Copy, Debug)]
pub struct WheelScroll {
    pub target: Pixels,
    /// 这一串滚轮的最后一格何时到的（滑太久就放弃，防止目标够不着时一直排帧）
    pub started: Instant,
    pub last: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模拟一段时间里的逐帧推进：返回每帧的步长。和 `ChatApp::step_wheel` 一致——
    /// 一步都没走出来（高刷尾段不到半个物理像素）时不调 `consume`。
    fn run_wheel(
        w: &mut WheelInput,
        start: Instant,
        tick: Duration,
        frames: usize,
        scale: f32,
    ) -> Vec<f32> {
        (1..=frames)
            .map(|i| {
                let at = start + tick * i as u32;
                let step = w.advance(at, None, tick, scale);
                if step != px(0.) {
                    w.consume(step, at);
                }
                step.as_f32()
            })
            .collect()
    }

    /// 单格 96px 在 60Hz 下缓起缓停，总位移精确守恒。
    #[core::prelude::v1::test]
    fn wheel_single_notch_eases_in_and_out_at_60hz() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(96.));
        let frame = Duration::from_secs_f32(1. / 60.);
        let steps = run_wheel(&mut w, start, frame, 30, 1.);
        let moved = steps.iter().rposition(|step| *step > 0.).unwrap() + 1;
        assert!((11..=13).contains(&moved), "走完用了 {moved} 帧");
        assert!((1. ..=2.).contains(&steps[0]), "首帧 {}", steps[0]);
        let peak = steps.iter().copied().fold(0f32, f32::max);
        assert!((peak - 14.).abs() <= 1., "巡航峰 {peak}");
        assert!(steps.iter().all(|step| *step >= 0.), "全程同向");
        for (first, last) in steps[..moved].iter().zip(steps[..moved].iter().rev()) {
            assert!((first - last).abs() <= 1., "步长破坏对称性：{steps:?}");
        }
        assert!((steps.iter().sum::<f32>() - 96.).abs() < 0.05);
        assert!(w.is_done(start + Duration::from_millis(500)));
    }

    #[core::prelude::v1::test]
    fn wheel_inverse_delta_duration_shortens_with_backlog_and_keeps_inherited_velocity() {
        let now = Instant::now();
        for (distance, duration_ms) in
            [(96., 200), (120., 200), (300., 150), (480., 100), (800., 100)]
        {
            let mut wheel = WheelInput::new(now, px(distance));
            assert!(
                wheel.seg_dur.abs_diff(Duration::from_millis(duration_ms))
                    < Duration::from_micros(1)
            );
            for inherited in [900., 4_000.] {
                wheel.plan_segment(inherited);
                assert!((0. ..=1.).contains(&wheel.y1));
                assert_eq!((WHEEL_X1, WHEEL_X2), (0.42, 0.58));
                let (start, slope) = cubic_bezier_y_and_slope(0., WHEEL_X1, wheel.y1, WHEEL_X2);
                let (_, end_slope) = cubic_bezier_y_and_slope(1., WHEEL_X1, wheel.y1, WHEEL_X2);
                assert_eq!((start, end_slope), (0., 0.));
                assert!((slope * distance / wheel.seg_dur.as_secs_f32() - inherited).abs() < 0.01);
            }
        }
    }

    /// 60 / 120 / 144 / 180 / 240 Hz 下，单格（96）和正反四格（±384）的总位移都严格等于输入，
    /// 且每帧都是整数物理像素。
    #[core::prelude::v1::test]
    fn wheel_distance_matches_input_at_any_refresh_rate() {
        for hz in [60u32, 120, 144, 180, 240] {
            let frame = Duration::from_secs_f32(1. / hz as f32);
            let frames = (hz as usize).max(60);
            let start = Instant::now();
            let mut one = WheelInput::new(start, px(96.));
            let total: f32 = run_wheel(&mut one, start, frame, frames, 1.).iter().sum();
            assert!((total - 96.).abs() <= 0.1, "{hz}Hz 单格走了 {total}");

            let mut four = WheelInput::new(start, px(96.));
            for _ in 0..3 {
                four.push(start, px(96.));
            }
            let steps = run_wheel(&mut four, start, frame, frames, 1.);
            let total: f32 = steps.iter().sum();
            assert!((total - 384.).abs() <= 0.1, "{hz}Hz 四格走了 {total}");
            assert!(steps.iter().all(|s| *s >= 0.), "{hz}Hz 出现反向帧");

            let mut up = WheelInput::new(start, px(-96.));
            for _ in 0..3 {
                up.push(start, px(-96.));
            }
            let steps = run_wheel(&mut up, start, frame, frames, 1.);
            let total: f32 = steps.iter().sum();
            assert!((total + 384.).abs() <= 0.1, "{hz}Hz 上滚四格走了 {total}");
            assert!(steps.iter().all(|s| *s <= 0.), "{hz}Hz 上滚出现反向帧");
        }
    }

    /// 极小步长（10px）：高刷每帧增量也很小，照样完整走完不丢位移。
    #[core::prelude::v1::test]
    fn wheel_tiny_step_survives_sub_pixel_frames() {
        for hz in [60u32, 240] {
            let frame = Duration::from_secs_f32(1. / hz as f32);
            let start = Instant::now();
            let mut w = WheelInput::new(start, px(10.));
            let total: f32 =
                run_wheel(&mut w, start, frame, (hz as usize).max(60), 1.).iter().sum();
            assert!((total - 10.).abs() <= 0.05, "{hz}Hz 10px 只走了 {total}");
        }
    }

    /// 紧邻首帧（1ms、2ms、4ms）：首帧固定按一个刷新周期起步，到达相位不影响首步和总量。
    #[core::prelude::v1::test]
    fn wheel_adjacent_first_frames_keep_their_share() {
        let mut firsts = Vec::new();
        for dt in [1u64, 2, 4] {
            let start = Instant::now();
            let mut w = WheelInput::new(start, px(96.));
            let mut total = px(0.);
            let mut at = start + Duration::from_millis(dt);
            let first = w.advance(at, None, DEFAULT_FRAME_DT, 1.);
            w.consume(first, at);
            total += first;
            firsts.push(first);
            for _ in 0..3 {
                at += Duration::from_millis(dt);
                let step = w.advance(at, None, DEFAULT_FRAME_DT, 1.);
                if step != px(0.) {
                    w.consume(step, at);
                }
                total += step;
            }
            for _ in 0..30 {
                at += Duration::from_millis(16);
                let step = w.advance(at, None, DEFAULT_FRAME_DT, 1.);
                if step != px(0.) {
                    w.consume(step, at);
                }
                total += step;
            }
            assert!((total.as_f32() - 96.).abs() <= 0.1, "dt={dt}ms 总计 {total:?}");
        }
        // 三者的首步完全一致（不再随事件到达时刻在 1–18px 之间乱跳）
        assert!(firsts.iter().all(|f| *f == firsts[0]), "首帧不一致：{firsts:?}");
        assert!((px(1.)..=px(3.)).contains(&firsts[0]), "首帧 {:?}", firsts[0]);
    }

    /// 杂重绘（同一刷新周期内的第二次唤醒）直接跳过：不推进节拍也不改总量。
    #[core::prelude::v1::test]
    fn wheel_ignores_spurious_redraws_inside_a_frame() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(96.));
        let mut at = start;
        let mut total = px(0.);
        // 首帧：固定按一个刷新周期起步。
        at += Duration::from_micros(600);
        let mut step = w.advance(at, None, DEFAULT_FRAME_DT, 1.);
        assert!((px(1.)..=px(2.)).contains(&step));
        w.consume(step, at);
        total += step;
        // 同一刷新周期里的杂重绘：一律 0，且不推进曲线时间
        for _ in 0..2 {
            at += Duration::from_micros(500);
            assert_eq!(w.advance(at, None, DEFAULT_FRAME_DT, 1.), px(0.), "杂重绘不该走位移");
        }
        // 抖动的唤醒间隔（13 / 20 / 14 / 19ms）都折合到整数倍刷新周期，逐帧照常推进
        for dt in [13u64, 20, 14, 19, 16, 17, 15] {
            at += Duration::from_millis(dt);
            step = w.advance(at, None, DEFAULT_FRAME_DT, 1.);
            if step != px(0.) {
                w.consume(step, at);
            }
            total += step;
        }
        for _ in 0..20 {
            at += Duration::from_millis(16);
            step = w.advance(at, None, DEFAULT_FRAME_DT, 1.);
            if step != px(0.) {
                w.consume(step, at);
            }
            total += step;
        }
        assert!((total.as_f32() - 96.).abs() <= 0.05, "总计 {total:?}");
    }

    /// 每秒 10 格连滚时检查相邻帧的速度变化。
    #[core::prelude::v1::test]
    fn wheel_continuous_rolling_has_no_sawtooth_spike() {
        let start = Instant::now();
        let frame = Duration::from_millis(16);
        let mut w = WheelInput::new(start, px(96.));
        let mut pushed = 1;
        let mut steps = Vec::new();
        for i in 1..=60u32 {
            let at = start + frame * i;
            while pushed < 4 && at >= start + Duration::from_millis(100 * pushed as u64) {
                w.push(start + Duration::from_millis(100 * pushed as u64), px(96.));
                pushed += 1;
            }
            let step = w.advance(at, None, frame, 1.);
            if step != px(0.) {
                w.consume(step, at);
            }
            steps.push(step.as_f32());
        }
        assert_eq!(pushed, 4);
        assert!((steps.iter().sum::<f32>() - 384.).abs() <= 0.1);
        // 巡航段（第 4–24 帧）不许出现「一格一冲」的锯齿
        let cruise = &steps[3..24];
        for pair in cruise.windows(2) {
            let fastest = pair[0].max(pair[1]);
            let slowest = pair[0].min(pair[1]);
            assert!(slowest > 0. && fastest / slowest < 2.0, "巡航相邻帧 {pair:?}");
        }
    }

    #[core::prelude::v1::test]
    fn wheel_slow_and_medium_rolling_stays_steady_at_180hz() {
        let frame = Duration::from_secs_f32(1. / 180.);
        for (interval_ms, peak_ratio) in [(120, 2.), (160, 3.)] {
            let start = Instant::now();
            let interval = Duration::from_millis(interval_ms);
            let mut wheel = WheelInput::new(start, px(96.));
            let mut pushed = 1;
            let mut min = f32::INFINITY;
            let mut max = 0f32;
            let mut total = px(0.);
            for i in 1..=180 {
                let at = start + frame * i;
                if pushed < 5 && at >= start + interval * pushed {
                    wheel.push(start + interval * pushed, px(96.));
                    pushed += 1;
                    assert!((0. ..=1.).contains(&wheel.y1));
                }
                let step = wheel.advance(at, Some(at), frame, 1.5);
                if step != px(0.) {
                    wheel.consume(step, at);
                    total += step;
                }
                if (3..5).contains(&pushed) {
                    min = min.min(step.as_f32());
                    max = max.max(step.as_f32());
                }
            }
            assert_eq!(pushed, 5);
            assert!((total - px(480.)).abs() < px(0.05));
            assert!(min > 0. && max / min < peak_ratio, "{interval_ms}ms: {min}..{max}");
        }
    }

    #[core::prelude::v1::test]
    fn wheel_single_notch_at_180hz_has_short_clean_tail() {
        let start = Instant::now();
        let frame = Duration::from_secs_f32(1. / 180.);
        let mut wheel = WheelInput::new(start, px(96.));
        let mut steps = Vec::new();
        for i in 1..=45 {
            let at = start + frame * i;
            let step = wheel.advance(at, Some(at), frame, 1.5);
            if step != px(0.) {
                wheel.consume(step, at);
            }
            steps.push(step.as_f32());
            if wheel.is_done(at) {
                break;
            }
        }
        let peak = steps.iter().enumerate().max_by(|(_, a), (_, b)| a.total_cmp(b)).unwrap().0;
        let tail_idle = steps[peak..].iter().filter(|&&step| step == 0.).count();
        assert!(tail_idle <= 2, "尾段空走 {tail_idle} 帧：{steps:?}");
        assert!((34..=36).contains(&steps.len()), "单格用了 {} 帧", steps.len());
        assert!((steps.iter().sum::<f32>() - 96.).abs() < 0.05);
    }

    #[core::prelude::v1::test]
    fn wheel_fast_spin_stops_crisply_after_last_notch() {
        let start = Instant::now();
        let frame = Duration::from_secs_f32(1. / 180.);
        let interval = Duration::from_millis(35);
        let last_input = start + interval * 9;
        let mut wheel = WheelInput::new(start, px(96.));
        let mut pushed = 1;
        let mut total = px(0.);
        for i in 1..=180 {
            let at = start + frame * i;
            if pushed < 10 && at >= start + interval * pushed {
                wheel.push(start + interval * pushed, px(96.));
                pushed += 1;
            }
            let step = wheel.advance(at, Some(at), frame, 1.5);
            if step != px(0.) {
                wheel.consume(step, at);
                total += step;
            }
            if pushed == 10 && wheel.is_done(at) {
                assert!(at - last_input <= Duration::from_millis(190), "{:?}", at - last_input);
                assert!((total - px(960.)).abs() < px(0.05));
                return;
            }
        }
        panic!("连滚没有停稳");
    }

    #[core::prelude::v1::test]
    fn wheel_vsync_clock_ignores_wm_paint_delivery_jitter_at_180hz() {
        let start = Instant::now();
        let frame = Duration::from_secs_f32(1. / 180.);
        let jitter = Duration::from_micros(3_500);
        let mut wheel = WheelInput::new(start, px(96.));
        // 第九次原生信号并入第十次，仍要补足真正错过的刷新周期。
        for i in (1..=16).filter(|&i| i != 9) {
            let vsync = start + frame * i;
            let delivery = vsync + if i % 2 == 0 { jitter } else { Duration::ZERO };
            let step = wheel.advance(delivery, Some(vsync), frame, 1.5);
            if step != px(0.) {
                wheel.consume(step, delivery);
            }
            assert_eq!(wheel.seg_elapsed, frame * i, "第 {i} 帧");
            assert_eq!(wheel.advance(vsync + jitter, None, frame, 1.5), px(0.));
            assert_eq!(wheel.seg_elapsed, frame * i, "同周期杂重绘");
        }
    }

    #[core::prelude::v1::test]
    fn wheel_fallback_hysteresis_absorbs_180hz_jitter() {
        let start = Instant::now();
        let frame = Duration::from_secs_f32(1. / 180.);
        let mut wheel = WheelInput::new(start, px(96.));
        let mut at = start;
        for (gap, count) in [(1., 1), (1.58, 2), (2., 4)] {
            at += frame.mul_f32(gap);
            let step = wheel.advance(at, None, frame, 1.5);
            if step != px(0.) {
                wheel.consume(step, at);
            }
            assert_eq!(wheel.seg_elapsed, frame * count, "间隔 {gap} 个周期");
        }
    }

    /// 任意 DPI 缩放 × 任意刷新率：每帧步长都落在物理像素网格上，总量严格守恒。
    #[core::prelude::v1::test]
    fn wheel_quantizes_to_physical_pixels_at_all_scales() {
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            for hz in [60u32, 120, 144, 180, 240] {
                let frame = Duration::from_secs_f32(1. / hz as f32);
                let start = Instant::now();
                let mut w = WheelInput::new(start, px(96.));
                let steps = run_wheel(&mut w, start, frame, (hz as usize).max(60), scale);
                for step in &steps {
                    let physical = step * scale;
                    assert!(
                        (physical - physical.round()).abs() < 1e-3,
                        "scale={scale} {hz}Hz 出现亚物理像素步长 {step}"
                    );
                }
                let total: f32 = steps.iter().sum();
                assert!((total - 96.).abs() <= 0.05, "scale={scale} {hz}Hz 总计 {total}");
            }
        }
    }

    /// 同向错峰输入（0 / 20 / 50ms 各一格 96px）直接累进余量，总计仍是 288。
    #[core::prelude::v1::test]
    fn wheel_staggered_same_direction_inputs_add_up() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(96.));
        let marks = [Duration::ZERO, Duration::from_millis(20), Duration::from_millis(50)];
        let mut pushed = 1;
        let mut total = px(0.);
        for i in 1..=35 {
            let at = start + Duration::from_millis(16) * i;
            while pushed < marks.len() && start + marks[pushed] <= at {
                w.push(start + marks[pushed], px(96.));
                pushed += 1;
            }
            let step = w.advance(at, None, Duration::from_millis(16), 1.);
            if step != px(0.) {
                w.consume(step, at);
            }
            total += step;
            assert!(step >= px(0.), "出现反向帧：{step:?}");
        }
        assert_eq!(pushed, marks.len());
        assert!((total.as_f32() - 288.).abs() <= 0.1, "总计 {total:?}");
    }

    /// 长帧（100ms 空档）：单帧不超过 48px，剩下的随后平顺走完。
    #[core::prelude::v1::test]
    fn wheel_long_frame_is_capped_and_finished_next() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(600.));
        let mut at = start + Duration::from_millis(100);
        let first = w.advance(at, None, DEFAULT_FRAME_DT, 1.);
        assert!(first <= px(48.), "单帧 {first:?} 超过上限");
        w.consume(first, at);
        let mut total = first;
        for _ in 0..50 {
            at += Duration::from_millis(16);
            let step = w.advance(at, None, DEFAULT_FRAME_DT, 1.);
            assert!(step <= px(48.), "限速帧超上限 {step:?}");
            if step != px(0.) {
                w.consume(step, at);
            }
            total += step;
        }
        assert!((total.as_f32() - 600.).abs() <= 0.1, "总计 {total:?}");
        assert!(w.is_done(at));
    }

    /// 停顿太久（去了设置页 / 最小化）回来时不能把几秒前的滚轮一次性补上。
    #[core::prelude::v1::test]
    fn wheel_is_dropped_after_a_long_gap() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(240.));
        let later = start + WHEEL_STALE;
        assert_eq!(w.advance(later, None, DEFAULT_FRAME_DT, 1.), px(0.));
        assert_eq!(w.remaining(), px(0.));
        assert!(w.is_done(later));
        // 之后的输入照常走
        w.push(later, px(96.));
        let total: f32 = run_wheel(&mut w, later, Duration::from_millis(16), 30, 1.).iter().sum();
        assert!((total - 96.).abs() <= 0.1, "恢复后的输入只走了 {total}");
    }

    /// 同向输入累进余量，反向输入清空旧方向余量。
    #[core::prelude::v1::test]
    fn wheel_reverse_clears_pending_distance() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(96.));
        let s1 = w.advance(start + Duration::from_millis(16), None, DEFAULT_FRAME_DT, 1.);
        w.consume(s1, start + Duration::from_millis(16));
        // 还没走完就往回滚一格
        w.push(start + Duration::from_millis(16), px(-96.));
        assert_eq!(w.remaining(), px(-96.), "反向时旧欠量应当清空并重置为新输入");
        let back = w.advance(start + Duration::from_millis(32), None, DEFAULT_FRAME_DT, 1.);
        assert!(back < px(0.), "换向后第一帧就朝新方向：{back:?}");
        w.consume(back, start + Duration::from_millis(32));
        // 走完这一格的总位移正好是 -96（不带上一格剩下的）
        let mut total = back;
        let mut at = start + Duration::from_millis(32);
        for _ in 0..30 {
            at += Duration::from_millis(16);
            let step = w.advance(at, None, DEFAULT_FRAME_DT, 1.);
            if step != px(0.) {
                w.consume(step, at);
            }
            total += step;
        }
        assert!((total.as_f32() + 96.).abs() <= 0.1, "换向后总位移 {total:?}");
    }

    /// 到边界：朝外的欠量作废，不再排帧；一直走不出去时在 WHEEL_HOLD 之后丢掉。
    #[core::prelude::v1::test]
    fn wheel_stops_at_boundary_or_after_hold() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(-96.));
        let at = start + Duration::from_millis(16);
        let step = w.advance(at, None, DEFAULT_FRAME_DT, 1.);
        // 顶部：一步也没走（`apply_relative` 返回 0，于是判到头），朝这个方向的余量作废
        assert!(step < px(0.));
        w.consume(px(0.), at);
        w.block(step > px(0.));
        assert_eq!(w.remaining(), px(0.));
        let mut now = at;
        for _ in 0..30 {
            now += Duration::from_millis(16);
            assert_eq!(w.advance(now, None, DEFAULT_FRAME_DT, 1.), px(0.));
        }
        assert!(w.is_done(now));

        // 欠量一直走不出去（列表放不出这段高度）：等到 WHEEL_HOLD 之后收掉
        let mut w = WheelInput::new(start, px(600.));
        let mut now = start;
        for _ in 0..10 {
            now += Duration::from_millis(16);
            let planned = w.advance(now, None, DEFAULT_FRAME_DT, 1.);
            assert!(planned > px(0.));
            w.consume(px(0.), now);
        }
        assert!(!w.is_done(now), "刚等一会儿不该放弃");
        let later = now + WHEEL_HOLD;
        assert!(w.expired(later) && w.is_done(later), "等太久应当放弃余量");
    }

    /// 卡顿期同向再滚一格：等待窗口重新起算，不沿用旧的 stalled_at。
    #[core::prelude::v1::test]
    fn wheel_push_resets_the_stall_timer() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(600.));
        let mut now = start;
        for _ in 0..20 {
            now += Duration::from_millis(16);
            w.advance(now, None, DEFAULT_FRAME_DT, 1.);
            w.consume(px(0.), now);
        }
        assert!(!w.expired(now));
        w.push(now, px(96.));
        // 刚推入时不该已经过期
        assert!(!w.expired(now));
        assert!(!w.expired(now + WHEEL_HOLD / 2));
    }

    /// 隔了很久（最小化后恢复）同向再滚一格：旧串作废，这一格照常走完。
    #[core::prelude::v1::test]
    fn wheel_push_after_a_long_gap_starts_over() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(240.));
        // 期间一帧都没画（去了设置页 / 最小化），回来时同向再推一格
        let back = start + Duration::from_secs(3);
        w.push(back, px(96.));
        let total: f32 = run_wheel(&mut w, back, Duration::from_millis(16), 30, 1.).iter().sum();
        assert!((total - 96.).abs() <= 0.1, "恢复后的这一格走了 {total}");
    }

    /// 卡住 10 帧之后解堵：进度没有被偷偷推进，首步仍是起手大小，不会一次性吐出全部欠量。
    #[core::prelude::v1::test]
    fn wheel_resumes_smoothly_after_unblocking() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(96.));
        let mut now = start;
        // 首帧走了一步就撞上边界：后面 10 帧一步都走不出去
        let first = w.advance(now, None, DEFAULT_FRAME_DT, 1.);
        assert!((px(1.)..=px(2.)).contains(&first));
        w.consume(px(0.), now);
        for _ in 0..10 {
            now += DEFAULT_FRAME_DT;
            let planned = w.advance(now, None, DEFAULT_FRAME_DT, 1.);
            assert!(planned > px(0.), "欠量还在，每帧都要有计划");
            w.consume(px(0.), now);
        }
        assert_eq!(w.remaining().as_f32(), 96., "一步都没走成，欠量原封不动");
        // 解堵：位移应与初次起步一致。
        now += DEFAULT_FRAME_DT;
        let resumed = w.advance(now, None, DEFAULT_FRAME_DT, 1.);
        assert_eq!(resumed, first, "解堵首步 {resumed:?}");
    }

    /// 极小残留（浮点减法的零头）当场算走完：`advance` / `consume` / `is_done` 用同一个阈值，
    /// 否则 `advance` 早退、`is_done` 不认，每帧空排一帧动画请求。
    #[core::prelude::v1::test]
    fn wheel_tiny_remainder_counts_as_done() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(5e-5));
        assert_eq!(w.advance(start, None, DEFAULT_FRAME_DT, 1.), px(0.));
        assert_eq!(w.remaining(), px(0.));
        assert!(w.is_done(start), "极小残留要当场算走完");

        // 走完最后一步只剩零头时也一样
        let mut w = WheelInput::new(start, px(96.));
        let mut now = start;
        loop {
            now += DEFAULT_FRAME_DT;
            let step = w.advance(now, None, DEFAULT_FRAME_DT, 1.);
            if step == px(0.) {
                break;
            }
            w.consume(step, now);
            if w.is_done(now) {
                break;
            }
        }
        assert!(w.is_done(now));
        assert_eq!(w.remaining(), px(0.), "不留零头");
    }

    /// 限速帧不叠旧误差：非整数缩放下也不会因为四舍五入越过 `WHEEL_STEP_MAX`。
    #[core::prelude::v1::test]
    fn wheel_capped_frames_never_exceed_the_step_limit() {
        for scale in [1.0f32, 1.25, 1.5, 1.75] {
            let start = Instant::now();
            let mut w = WheelInput::new(start, px(2000.));
            let mut now = start;
            let mut total = px(0.);
            for _ in 0..400 {
                now += DEFAULT_FRAME_DT;
                let step = w.advance(now, None, DEFAULT_FRAME_DT, scale);
                if step == px(0.) {
                    continue;
                }
                assert!(step <= WHEEL_STEP_MAX, "scale={scale} 限速帧 {step:?} 越过上限");
                let physical = step.as_f32() * scale;
                assert!(
                    (physical - physical.round()).abs() < 1e-3,
                    "scale={scale} 出现亚物理像素步长 {step:?}"
                );
                w.consume(step, now);
                total += step;
                if w.is_done(now) {
                    break;
                }
            }
            assert!((total.as_f32() - 2000.).abs() <= 0.1, "scale={scale} 总计 {total:?}");
        }
    }
}
