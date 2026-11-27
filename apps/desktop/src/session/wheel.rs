//! 对话滚轮按格生成位移，列表确认实际消费后才提交。

use gpui_kit::*;
use std::time::{Duration, Instant};

const SINGLE_NOTCH: Duration = Duration::from_millis(184);
const SHORT_NOTCH: Duration = Duration::from_millis(120);
const WHEEL_MAX_SPEED: f64 = 6000.;
const WHEEL_STEP_MAX: Pixels = px(48.);
const WHEEL_HOLD: Duration = Duration::from_millis(400);
const WHEEL_STALE: Duration = Duration::from_millis(500);
const DISTANCE_EPSILON: f64 = 1e-4;
pub const DEFAULT_FRAME_DT: Duration = Duration::from_micros(16_667);

/// 用本机 Kimi 的单格累计位移拟合贝塞尔 (0.40, 0.03, 0.60, 1.0)。
fn notch_progress(fraction: f64) -> f64 {
    if fraction <= 0. || fraction >= 1. {
        return fraction.clamp(0., 1.);
    }
    let mut parameter = fraction;
    for _ in 0..4 {
        let curve_x = parameter * (1.2 + parameter * (-0.6 + 0.4 * parameter));
        let derivative = 1.2 + parameter * (-1.2 + 1.2 * parameter);
        parameter -= (curve_x - fraction) / derivative;
    }
    parameter * (0.09 + parameter * (2.82 - 1.91 * parameter))
}

/// 用前一格的输入间隔校准收尾时长。已经开始的曲线保持原样。
fn notch_duration(interval: Duration) -> Duration {
    let speed = ((160. - interval.as_secs_f64() * 1000.) / 80.).clamp(0., 1.);
    SINGLE_NOTCH - Duration::from_secs_f64(0.028 * speed * speed)
}

#[derive(Clone, Copy, Debug)]
struct WheelNotch {
    start: Duration,
    duration: Duration,
    distance: f64,
}

impl WheelNotch {
    fn released(self, sample: Duration) -> f64 {
        let age = sample.saturating_sub(self.start);
        self.distance * notch_progress(age.as_secs_f64() / self.duration.as_secs_f64())
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingStep {
    elapsed: Duration,
    distance: Pixels,
}

/// 同向输入叠加独立曲线。列表位置和行高变化不参与生成曲线。
#[derive(Clone, Debug, Default)]
pub struct WheelInput {
    notches: Vec<WheelNotch>,
    retired: f64,
    requested: f64,
    applied: f64,
    sign: f32,
    elapsed: Duration,
    last_input: Option<Instant>,
    last: Option<Instant>,
    frame_interval: Option<Duration>,
    last_was_vsync: bool,
    started_frame: bool,
    stalled_at: Option<Instant>,
    pending: Option<PendingStep>,
}

impl WheelInput {
    pub fn new(now: Instant, delta: Pixels) -> Self {
        let mut wheel = Self::default();
        wheel.push(now, delta);
        wheel
    }

    pub fn push(&mut self, now: Instant, delta: Pixels) {
        self.push_notch(now, delta, None);
    }

    pub fn push_short(&mut self, now: Instant, delta: Pixels) {
        self.push_notch(now, delta, Some(SHORT_NOTCH));
    }

    fn push_notch(&mut self, now: Instant, delta: Pixels, duration: Option<Duration>) {
        let delta = delta.as_f32();
        if delta == 0. || !delta.is_finite() {
            return;
        }
        let sign = delta.signum();
        if self.is_done(now)
            || self.sign != sign
            || self.last.is_some_and(|last| now.saturating_duration_since(last) >= WHEEL_STALE)
        {
            self.clear();
            self.sign = sign;
            self.last = Some(now);
        }
        let offset = self
            .last
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last))
            .min(self.frame_interval.unwrap_or(DEFAULT_FRAME_DT));
        let duration = duration.unwrap_or_else(|| {
            self.last_input
                .map_or(SINGLE_NOTCH, |last| notch_duration(now.saturating_duration_since(last)))
        });
        let distance = f64::from(delta.abs());
        let start = if self.stalled_at.is_some() { self.elapsed } else { self.elapsed + offset };
        self.notches.push(WheelNotch { start, duration, distance });
        self.requested += distance;
        self.last_input = Some(now);
        self.stalled_at = None;
    }

    pub fn advance(
        &mut self,
        now: Instant,
        vsync_at: Option<Instant>,
        frame_dt: Duration,
        scale: f32,
    ) -> Pixels {
        self.pending = None;
        if self.is_done(now) {
            self.clear();
            return px(0.);
        }
        let frame_dt = if frame_dt.is_zero() { DEFAULT_FRAME_DT } else { frame_dt };
        self.frame_interval = Some(frame_dt);
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

        let next_elapsed = self.elapsed + paced_dt;
        let released = self.retired
            + self.notches.iter().map(|notch| notch.released(next_elapsed)).sum::<f64>();
        let remaining = self.requested - self.applied;
        let due = (released - self.applied).clamp(0., remaining);
        let scale = f64::from(scale);
        let max_step = (WHEEL_MAX_SPEED * paced_dt.as_secs_f64())
            .max(1. / scale)
            .min(f64::from(WHEEL_STEP_MAX.as_f32()));
        let max_aligned = (max_step * scale).floor() / scale;
        let distance = if remaining <= max_step && remaining - due <= DISTANCE_EPSILON {
            remaining
        } else {
            ((due * scale).round() / scale).min(max_aligned).min(remaining)
        };
        let step = px(self.sign * distance as f32);
        if step == px(0.) {
            self.commit_time(next_elapsed);
        } else {
            self.pending = Some(PendingStep { elapsed: next_elapsed, distance: step });
        }
        step
    }

    fn commit_time(&mut self, elapsed: Duration) {
        self.elapsed = elapsed;
        let retired = &mut self.retired;
        self.notches.retain(|notch| {
            if elapsed >= notch.start + notch.duration {
                *retired += notch.distance;
                false
            } else {
                true
            }
        });
    }

    pub fn consume(&mut self, applied: Pixels, now: Instant) {
        let Some(pending) = self.pending.take() else { return };
        let progress = f64::from(self.sign * applied.as_f32())
            .clamp(0., f64::from(pending.distance.as_f32().abs()));
        self.applied += progress;
        if self.requested - self.applied <= DISTANCE_EPSILON {
            self.clear();
        } else if progress == 0. {
            self.stalled_at.get_or_insert(now);
        } else {
            self.stalled_at = None;
            if progress + DISTANCE_EPSILON >= f64::from(pending.distance.as_f32().abs()) {
                self.commit_time(pending.elapsed);
            }
        }
    }

    pub fn clear(&mut self) {
        let mut notches = std::mem::take(&mut self.notches);
        notches.clear();
        *self = Self { notches, ..Self::default() };
    }

    pub fn block(&mut self, down: bool) {
        if self.sign != 0. && (self.sign > 0.) == down {
            self.clear();
        }
    }

    pub fn expired(&self, now: Instant) -> bool {
        self.stalled_at.is_some_and(|at| now.saturating_duration_since(at) >= WHEEL_HOLD)
    }

    pub fn is_done(&self, now: Instant) -> bool {
        self.requested - self.applied <= DISTANCE_EPSILON || self.expired(now)
    }

    #[cfg(test)]
    pub fn remaining(&self) -> Pixels {
        px(self.sign * (self.requested - self.applied) as f32)
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

    #[core::prelude::v1::test]
    fn wheel_short_notch_settles_sooner_without_a_higher_peak() {
        for hz in [60, 180] {
            let start = Instant::now();
            let frame = Duration::from_secs_f64(1. / f64::from(hz));
            let mut smooth = WheelInput::new(start, px(110.4));
            let mut short = WheelInput::default();
            short.push_short(start, px(64.));
            let smooth = run_wheel(&mut smooth, start, frame, 40, 1.5);
            let short = run_wheel(&mut short, start, frame, 40, 1.5);
            assert!(short.iter().all(|step| *step >= 0.));
            assert!(short.iter().filter(|step| **step > 0.).count() >= 4);
            assert!((short.iter().sum::<f32>() - 64.).abs() < 0.05);
            assert!(
                short.iter().rposition(|step| *step > 0.)
                    < smooth.iter().rposition(|step| *step > 0.)
            );
            let short_peak = short.into_iter().fold(0., f32::max);
            let smooth_peak = smooth.into_iter().fold(0., f32::max);
            assert!(short_peak <= smooth_peak, "{hz}Hz: {short_peak} > {smooth_peak}");
        }
    }

    /// 单格在前 50ms 要看得出位移，同时限制首帧和峰值，避免一格猛冲。
    #[core::prelude::v1::test]
    fn wheel_single_notch_responds_promptly_at_60hz() {
        let start = Instant::now();
        let mut w = WheelInput::new(start, px(96.));
        let frame = Duration::from_secs_f32(1. / 60.);
        let steps = run_wheel(&mut w, start, frame, 30, 1.);
        let moved = steps.iter().rposition(|step| *step > 0.).unwrap() + 1;
        assert!((11..=13).contains(&moved), "走完用了 {moved} 帧");
        assert!((2. ..=4.).contains(&steps[0]), "首帧 {}", steps[0]);
        assert!((14. ..=20.).contains(&steps[..3].iter().sum::<f32>()), "前 50ms {steps:?}");
        let peak = steps.iter().copied().fold(0f32, f32::max);
        assert!((12. ..=16.).contains(&peak), "巡航峰 {peak}");
        assert!(steps.iter().all(|step| *step >= 0.), "全程同向");
        assert!(steps[moved - 1] < steps[moved - 2] && steps[moved - 1] <= steps[0]);
        assert!((steps.iter().sum::<f32>() - 96.).abs() < 0.05);
        assert!(w.is_done(start + Duration::from_millis(500)));
    }

    #[core::prelude::v1::test]
    fn wheel_notch_duration_tracks_the_input_interval() {
        for (interval_ms, duration_ms) in [(35, 156), (80, 156), (120, 177), (160, 184)] {
            assert_eq!(
                notch_duration(Duration::from_millis(interval_ms)),
                Duration::from_millis(duration_ms)
            );
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
        assert!(firsts[0] > px(0.), "首帧 {:?}", firsts[0]);
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
        assert!(step > px(0.));
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
    fn wheel_slow_and_medium_rolling_stays_steady() {
        for (hz, scale, window_frames) in [(60, 1., 2), (180, 1.5, 4)] {
            let frame = Duration::from_secs_f32(1. / hz as f32);
            for (interval_ms, peak_ratio) in [(80, 1.75), (120, 1.9), (160, 4.2)] {
                let start = Instant::now();
                let interval = Duration::from_millis(interval_ms);
                let mut wheel = WheelInput::new(start, px(96.));
                let mut pushed = 1;
                let mut cruise = Vec::new();
                let mut total = px(0.);
                for i in 1..=hz * 3 {
                    let at = start + frame * i;
                    if pushed < 14 && at >= start + interval * pushed {
                        wheel.push(start + interval * pushed, px(96.));
                        pushed += 1;
                    }
                    let step = wheel.advance(at, Some(at), frame, scale);
                    if step != px(0.) {
                        wheel.consume(step, at);
                        total += step;
                    }
                    if (6..14).contains(&pushed) {
                        cruise.push(step.as_f32());
                    }
                }
                assert_eq!(pushed, 14);
                assert!((total - px(1344.)).abs() < px(0.05));
                let fastest = cruise.iter().copied().fold(0., f32::max);
                assert!(
                    fastest <= 3. * 1000. / interval_ms as f32 * 96. / hz as f32,
                    "{hz}Hz {interval_ms}ms: {fastest}"
                );
                // 在 22–33ms 内累计位移，再比较速度，减小物理像素取整的影响。
                let distances =
                    cruise.windows(window_frames).map(|frames| frames.iter().sum::<f32>());
                let min = distances.clone().fold(f32::INFINITY, f32::min);
                let max = distances.fold(0., f32::max);
                assert!(max / min < peak_ratio, "{hz}Hz {interval_ms}ms: {min}..{max}");
            }
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
        assert!(steps[0] * 1.5 >= 1., "第一帧应有可见位移：{steps:?}");
        assert!(tail_idle <= 2, "尾段空走 {tail_idle} 帧：{steps:?}");
        assert!((32..=34).contains(&steps.len()), "单格用了 {} 帧", steps.len());
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
            assert_eq!(wheel.elapsed, frame * i, "第 {i} 帧");
            assert_eq!(wheel.advance(vsync + jitter, None, frame, 1.5), px(0.));
            assert_eq!(wheel.elapsed, frame * i, "同周期杂重绘");
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
            assert_eq!(wheel.elapsed, frame * count, "间隔 {gap} 个周期");
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

    /// 活动曲线中途推入下一格：旧曲线和待确认步长仍然有效。
    #[core::prelude::v1::test]
    fn wheel_push_updates_target_without_replaying_history() {
        let start = Instant::now();
        let mut wheel = WheelInput::new(start, px(96.));
        let at = start + DEFAULT_FRAME_DT * 5;
        let first = wheel.advance(at, Some(at), DEFAULT_FRAME_DT, 1.);
        wheel.consume(first, at);
        let first_notch = wheel.notches[0];
        let next = at + DEFAULT_FRAME_DT;
        let pending = wheel.advance(next, Some(next), DEFAULT_FRAME_DT, 1.);
        assert!(pending > px(0.));
        wheel.push(next, px(96.));
        assert_eq!(wheel.remaining(), px(192.) - first);
        assert_eq!(wheel.notches[0].start, first_notch.start);
        assert_eq!(wheel.notches[0].duration, first_notch.duration);
        assert_eq!(wheel.notches[0].distance, first_notch.distance);
        wheel.consume(pending, next);
        wheel.consume(pending, next);
        assert_eq!(wheel.remaining(), px(192.) - first - pending);
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
        assert!(first > px(0.));
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

    #[core::prelude::v1::test]
    fn wheel_partial_consumption_preserves_progress_and_distance() {
        let start = Instant::now();
        let mut wheel = WheelInput::new(start, px(96.));
        let next = start + DEFAULT_FRAME_DT;
        let planned = wheel.advance(next, Some(next), DEFAULT_FRAME_DT, 1.);
        assert!(planned > px(1.));
        wheel.consume(px(1.), next);
        assert_eq!(wheel.elapsed, Duration::ZERO);
        assert_eq!(wheel.remaining(), px(95.));
        let steps = run_wheel(&mut wheel, next, DEFAULT_FRAME_DT, 40, 1.);
        assert!((steps.iter().sum::<f32>() - 95.).abs() < 0.05);
    }

    #[core::prelude::v1::test]
    fn wheel_input_after_a_long_frame_starts_on_the_next_tick() {
        let start = Instant::now();
        let frame = Duration::from_secs_f32(1. / 180.);
        let mut wheel = WheelInput::new(start, px(96.));
        let first_frame = start + frame;
        let first = wheel.advance(first_frame, Some(first_frame), frame, 1.5);
        wheel.consume(first, first_frame);
        let delayed = first_frame + Duration::from_millis(300);
        wheel.push(delayed, px(96.));
        assert_eq!(wheel.notches[1].start, 2 * frame);
        let steps = run_wheel(&mut wheel, delayed, frame, 40, 1.5);
        assert!((steps.iter().sum::<f32>() + first.as_f32() - 192.).abs() < 0.05);
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
