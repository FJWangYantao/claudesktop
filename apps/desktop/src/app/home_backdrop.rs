//! 首页背景：地平线极光 + ASCII 波动字阵。
//!
//! 只在「还没发过消息的首页」挂载（见 `ChatApp::home_backdrop_active`）。一进对话
//! 就整块卸载：图元不再提交、显存由 `PrismGpuResources` 在 120 帧后回收，
//! 所以对话态的背景开销是 0 CPU / 0 GPU / 0 VRAM。
//!
//! 帧循环是**自调度**的，不是常驻：只有波纹还在跑、指针光晕还没消退、或者刚发过消息
//! 的脉冲还没播完时，才在 paint 里 `request_animation_frame()`。三者都停下之后
//! 一帧都不排——首页静止就是真的 0fps。
//!
//! 状态放在 `Rc<RefCell<HomeBackdrop>>` 里由 canvas 闭包持有（不借用 view），
//! 根视图的鼠标事件也只拿这个 `Rc`，所以指针移动不会让 `ChatApp` 变脏。
use crate::theme;
use gpui_kit::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::prism_rim::ambient_level;
use super::ripple::{CELL_H, CELL_W, GLYPHS, GLYPH_COUNT, PackParams, RippleField};

/// 指针停下之后光晕再留多久。
const AURA_LINGER: Duration = Duration::from_millis(1200);
/// 光晕的半径（逻辑像素）。
const AURA_RADIUS: f32 = 190.;
/// 指针要挪这么远才注入一次扰动（抖动不注入，省得把水面搅成噪点）。
const WAKE_MIN_MOVE: f32 = 7.;
/// 指针扫过注入的速度。
const WAKE_STRENGTH: f32 = 0.5;
/// 点击注入的波包强度。
const SPLASH_STRENGTH: f32 = 1.7;
/// 发消息瞬间的脉冲时长。
const SURGE_LINGER: Duration = Duration::from_millis(700);

/// 首页背景的全部可变状态。
pub(crate) struct HomeBackdrop {
    field: RippleField,
    cells: Option<Arc<AsciiCellsUpload>>,
    atlas: Option<Arc<AsciiAtlasUpload>>,
    /// 图集对应的（字体版本, 缩放比 * 100）：变了才重建
    atlas_key: Option<(u64, u32)>,
    /// 上一次注入扰动的位置（窗口坐标）
    last_wake: Option<Point<Pixels>>,
    /// 指针光晕：位置 + 最近一次移动的时刻
    aura: Option<(Point<Pixels>, Instant)>,
    /// 发消息的脉冲：起始时刻 + 起始强度
    surge: Option<(Instant, f32)>,
    /// 背景的窗口坐标（prepaint 写入，指针换算成局部坐标要用）
    bounds: Option<Bounds<Pixels>>,
    /// 计数开始时刻：极光的时间轴
    started: Instant,
    /// 调试钩子 `CLAUDESKTOP_PRISM_SEED` 的波包是否已经注入（保证只注一次）
    seeded: bool,
}

impl Default for HomeBackdrop {
    fn default() -> Self {
        Self {
            field: RippleField::new(),
            cells: None,
            atlas: None,
            atlas_key: None,
            last_wake: None,
            aura: None,
            surge: None,
            bounds: None,
            started: Instant::now(),
            seeded: false,
        }
    }
}

impl HomeBackdrop {
    /// 背景在窗口里的位置（还没画过时是 `None`）。
    pub(crate) fn bounds(&self) -> Option<Bounds<Pixels>> {
        self.bounds
    }

    /// 指针移动：注入扰动 + 更新光晕。抖动（位移小于阈值）只续光晕不扰动水面。
    pub(crate) fn pointer(&mut self, position: Point<Pixels>) {
        let Some(bounds) = self.bounds else { return };
        let local = (
            f32::from(position.x) - f32::from(bounds.origin.x),
            f32::from(position.y) - f32::from(bounds.origin.y),
        );
        if let Some(last) = self.last_wake {
            let previous = (
                f32::from(last.x) - f32::from(bounds.origin.x),
                f32::from(last.y) - f32::from(bounds.origin.y),
            );
            let moved = ((local.0 - previous.0).powi(2) + (local.1 - previous.1).powi(2)).sqrt();
            if moved >= WAKE_MIN_MOVE {
                self.field.wake(previous, local, WAKE_STRENGTH);
                self.last_wake = Some(position);
            }
        } else {
            self.last_wake = Some(position);
        }
        self.aura = Some((position, Instant::now()));
    }

    /// 首页空白处点一下：一个圆形波包。
    pub(crate) fn splash(&mut self, position: Point<Pixels>) {
        let Some(bounds) = self.bounds else { return };
        let local = (
            f32::from(position.x) - f32::from(bounds.origin.x),
            f32::from(position.y) - f32::from(bounds.origin.y),
        );
        self.field.splash(local.0, local.1, SPLASH_STRENGTH);
        self.aura = Some((position, Instant::now()));
    }

    /// 发出第一条消息时的脉冲（切走之前的最后一亮）。
    pub(crate) fn surge(&mut self) {
        self.surge = Some((Instant::now(), 1.));
    }

    /// 有没有东西还在动（决定要不要排下一帧）。
    pub(crate) fn animating(&self) -> bool {
        if self.field.is_active() || self.surge.is_some() {
            return true;
        }
        self.aura
            .is_some_and(|(_, at)| at.elapsed() < AURA_LINGER)
    }

    fn resize(&mut self, bounds: Bounds<Pixels>) {
        self.bounds = Some(bounds);
        self.field.resize(
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
        );
    }

    /// 这一帧的打包参数（光晕随指针停留时间缩小到消失）。
    fn pack_params(&self, bounds: Bounds<Pixels>) -> PackParams {
        let aura = self
            .aura
            .filter(|(_, at)| at.elapsed() < AURA_LINGER)
            .map(|(position, at)| {
                let age = at.elapsed().as_secs_f32() / AURA_LINGER.as_secs_f32();
                (
                    f32::from(position.x) - f32::from(bounds.origin.x),
                    f32::from(position.y) - f32::from(bounds.origin.y),
                    AURA_RADIUS * (1. - age).max(0.15),
                )
            });
        PackParams {
            wave_gain: 1.,
            cloud: 0.45,
            aura,
        }
    }

    /// 画这一层，并决定要不要排下一帧。
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window) {
        self.resize(bounds);
        let ambient = ambient_level();
        if ambient <= 0.001 {
            return;
        }

        let now = Instant::now();
        // 离屏截图的确定性入口：固定时间、固定波包、固定指针（见 `prism_seed`）
        let seed = prism_seed();
        if let Some((_, seed_x, seed_y)) = seed {
            if !self.seeded {
                self.seeded = true;
                self.field.splash(seed_x, seed_y, SPLASH_STRENGTH);
                // 光晕按窗口坐标存（`pack_params` 里再减掉左上角）
                self.aura = Some((
                    Point {
                        x: bounds.origin.x + px(seed_x),
                        y: bounds.origin.y + px(seed_y),
                    },
                    now,
                ));
                // 立刻把波推到一个固定相位，否则截图取决于等了多少帧
                for step in 0..18 {
                    self.field.step(self.started + Duration::from_millis(16 * step));
                }
            }
        } else {
            self.field.step(now);
        }
        let params = self.pack_params(bounds);
        if self.field.pack(&params) || self.cells.is_none() {
            let revision = self.field.revision();
            self.cells = Some(Arc::new(AsciiCellsUpload {
                width: self.field.cols() as u32,
                height: self.field.rows() as u32,
                revision,
                // 每帧拷一份 40KB：只有波纹在动的时候才走到这里，换「不拷贝」要多一层
                // 共享所有权，暂时不值得。
                rgba: self.field.pixels().to_vec(),
            }));
        }
        self.ensure_atlas(window);

        let dark = theme::is_dark();
        let seconds = seed.map_or_else(|| self.started.elapsed().as_secs_f32(), |s| s.0);
        let surge = self.surge.map_or(0., |(at, amount)| {
            let age = at.elapsed().as_secs_f32() / SURGE_LINGER.as_secs_f32();
            if age >= 1. { 0. } else { amount * (1. - age) }
        });
        if self
            .surge
            .is_some_and(|(at, _)| at.elapsed() >= SURGE_LINGER)
        {
            self.surge = None;
        }

        // 地平线极光：贴着底部，从下往上淡出
        window.paint_horizon_glow(
            bounds,
            HorizonGlowParams {
                time: seconds,
                dark,
                reach: 0.62,
                intensity: 0.42 * ambient,
                stretch: 1.,
                presence: 0.38,
                prism: 0.35,
                surge,
                speed: 1.,
                spectrum: 1.,
                semantic: semantic_hue(),
            },
        );

        // ASCII 波动字阵：字号 / 缩放变了才重建图集
        if let (Some(cells), Some(atlas)) = (self.cells.clone(), self.atlas.clone()) {
            let dpr = window.scale_factor().max(0.5);
            let atlas_cell = atlas.width as f32 / GLYPH_COUNT as f32;
            window.paint_ascii_ripple(
                bounds,
                AsciiRippleParams {
                    grid: [CELL_W, CELL_H, atlas_cell / dpr, GLYPH_COUNT as f32],
                    // 色值直接给 sRGB 分量：观感在 shader 里调，和界面其它地方一致
                    ink: with_alpha(theme::rgb(theme::INK_FAINT), 0.9),
                    wave: with_alpha(theme::rgb(theme::CORAL), 0.8),
                    aura: [
                        params.aura.map_or(0., |a| a.0),
                        params.aura.map_or(0., |a| a.1),
                        params.aura.map_or(0., |a| a.2),
                        1.,
                    ],
                    optics: [0.75, 0.12, 0., 0.6],
                    timing: [seconds, if dark { 1. } else { 0. }, 0.9 * ambient, seconds * 0.15],
                    nebula: [0.4, 0., 0.7, 0.35],
                    atlas_size: [
                        atlas.width as f32 / dpr,
                        atlas.height as f32 / dpr,
                        0.,
                        0.,
                    ],
                },
                Some(cells),
                Some(atlas),
            );
        }

        // 种子模式下不再排帧：截图要的就是这一刻的样子
        if seed.is_none() && self.animating() {
            window.request_animation_frame();
        }
    }

    /// 62 格字形图集：字体版本或缩放比变了才重建（约 16KB）。
    fn ensure_atlas(&mut self, window: &mut Window) {
        let dpr = window.scale_factor().max(0.5);
        let key = (theme::font_revision(), (dpr * 100.).round() as u32);
        if self.atlas.is_some() && self.atlas_key == Some(key) {
            return;
        }

        let cell_w = (CELL_W * dpr).round().max(1.) as i32;
        let cell_h = (CELL_H * dpr).round().max(1.) as i32;
        let width = (cell_w * GLYPH_COUNT as i32).max(1) as u32;
        let height = cell_h.max(1) as u32;
        let mut alpha = vec![0u8; width as usize * height as usize];

        let mono = font(theme::MONO);
        let text_system = window.text_system().clone();
        // 基线放在格子高度的 78%：等宽字体的小写字母下伸部刚好不越界
        let baseline = (cell_h as f32 * 0.78).round() as i32;
        for (index, ch) in GLYPHS.iter().enumerate() {
            let Ok((size, origin, bitmap)) =
                text_system.rasterize_mono_glyph(&mono, px(CELL_H), *ch as char, dpr)
            else {
                continue;
            };
            let (bw, bh) = (size.width.0, size.height.0);
            if bw <= 0 || bh <= 0 || bitmap.len() < (bw * bh) as usize {
                continue;
            }
            let cell_x = index as i32 * cell_w;
            for y in 0..bh {
                let dst_y = origin.y.0 + baseline + y;
                if dst_y < 0 || dst_y >= cell_h {
                    continue;
                }
                for x in 0..bw {
                    let dst_x = cell_x + origin.x.0 + x;
                    if dst_x < cell_x || dst_x >= cell_x + cell_w {
                        continue;
                    }
                    let src = y * bw + x;
                    let dst = dst_y as usize * width as usize + dst_x as usize;
                    if let Some(slot) = alpha.get_mut(dst) {
                        // 同一格多次落笔取最亮的，别叠加成实心块
                        *slot = (*slot).max(bitmap[src as usize]);
                    }
                }
            }
        }

        self.atlas = Some(Arc::new(AsciiAtlasUpload {
            width,
            height,
            key: atlas_key_hash(key),
            alpha_r8: alpha,
        }));
        self.atlas_key = Some(key);
    }
}

/// 调试钩子：`CLAUDESKTOP_PRISM_SEED=<秒,px,py>`。
///
/// 只在 `--features headless-screenshot` 的构建里生效：固定极光时间、注入一次固定
/// 位置的波包、把指针光晕钉在 `(px, py)`（相对背景左上，逻辑像素），这样离屏截图
/// 才能逐字节回归。正常构建里永远是 `None`。
fn prism_seed() -> Option<(f32, f32, f32)> {
    if !cfg!(feature = "headless-screenshot") {
        return None;
    }
    let raw = std::env::var("CLAUDESKTOP_PRISM_SEED").ok()?;
    let mut parts = raw.split(',');
    let seconds = parts.next()?.trim().parse().ok()?;
    let x = parts.next()?.trim().parse().ok()?;
    let y = parts.next()?.trim().parse().ok()?;
    Some((seconds, x, y))
}

/// 极光的语义色相：暖纸 / 琥珀那几套偏橙，其余偏冷。
fn semantic_hue() -> f32 {
    match theme::prism_tone() {
        1 | 4 => 0.88,
        5 => 0.2,
        _ => 0.28,
    }
}

fn with_alpha(color: Rgba, alpha: f32) -> [f32; 4] {
    [color.r, color.g, color.b, alpha]
}

fn atlas_key_hash(key: (u64, u32)) -> u64 {
    key.0
        .rotate_left(17)
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(key.1 as u64)
}

/// 首页背景这一层本身。
pub(crate) struct HomeBackdropView {
    backdrop: Rc<RefCell<HomeBackdrop>>,
}

impl HomeBackdropView {
    pub(crate) fn new(backdrop: Rc<RefCell<HomeBackdrop>>) -> Self {
        Self { backdrop }
    }
}

impl Render for HomeBackdropView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let backdrop = self.backdrop.clone();
        canvas(
            move |bounds, _, _| {
                backdrop.borrow_mut().bounds = Some(bounds);
            },
            move |bounds, _, window, _| {
                backdrop.borrow_mut().paint(bounds, window);
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

impl super::ChatApp {
    /// 首页背景该不该挂载：还没发过消息、没开设置页、氛围效果和动画都开着。
    ///
    /// 挂在 `content_slot` 里，一进对话这层就整块不挂载了（图元不再提交，
    /// 显存在 120 帧后由渲染器回收）。
    pub(super) fn home_backdrop_active(&self) -> bool {
        !self.settings_open
            && !super::motion::reduced()
            && super::prism_rim::ambient_on()
            && !self.session().has_user_message()
    }

    /// 指针移动：16ms 节流 + 只通知背景 view 自己（`ChatApp` 不变脏）。
    pub(super) fn track_home_pointer(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if !self.home_backdrop_active() {
            return;
        }
        let now = Instant::now();
        if self
            .home_pointer_at
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_millis(16))
        {
            return;
        }
        self.home_pointer_at = Some(now);
        let animating = {
            let mut backdrop = self.home_backdrop.borrow_mut();
            backdrop.pointer(position);
            backdrop.animating()
        };
        if animating {
            self.home_backdrop_view.update(cx, |_, cx| cx.notify());
        }
    }

    /// 首页空白处按下：丢一个波包。
    pub(super) fn home_splash(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.home_backdrop_active() {
            return;
        }
        self.home_backdrop.borrow_mut().splash(position);
        self.home_backdrop_view.update(cx, |_, cx| cx.notify());
    }
}
