//! 首页背景的 ASCII 波动场（CPU 侧）。
//!
//! 一个二维波动方程 + 分形速度场，按格子算能量，打包成一张 RGBA8 贴图交给
//! `ascii_ripple.hlsl` 铺字形（GPU 只负责取样和上色，不跑模拟）。
//!
//! 每格 4 个字节，含义和 shader 里一一对应：
//! - `R` 字形下标（0..[`GLYPH_COUNT`]），shader 里 `floor(r * 255 + 0.5)` 取回来；
//! - `G` 这一格的不透明度（静止态的云雾 + 波面抬升）；
//! - `B` ink → 波面色的混合比例；
//! - `A` 高 3 bit 棱镜强度 + 低 5 bit 色相（由波面梯度算）。
//!
//! 性能上的两个关键点：
//! 1. **活跃矩形**：只在有能量的那块矩形里算、也只更新那块的像素，静止时面积为零；
//! 2. **静止即归零**：能量低于 [`REST_HEIGHT`] 的格子直接清掉，整块低于阈值时
//!    返回 `false`，上层据此停掉帧循环（首页静止 = 0fps）。
use std::f32::consts::TAU;

/// 单元格的宽（逻辑像素）。和 `CELL_H` 一起决定字阵的疏密。
pub(crate) const CELL_W: f32 = 7.;
/// 单元格的高（逻辑像素）。
pub(crate) const CELL_H: f32 = 15.;
/// 字形数量（`0-9` + `A-Z` + `a-z` = 10 + 26 + 26 = 62）。
pub(crate) const GLYPH_COUNT: usize = 62;
/// 这一套字形，顺序就是图集里的格子顺序。
pub(crate) const GLYPHS: &[u8; GLYPH_COUNT] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 能量低于这个值就当作没有：直接归零，免得小数值一直让「活跃」永远成立。
const REST_HEIGHT: f32 = 0.003;
/// 边界几格额外强阻尼，波不会从边上弹回来（看起来像在玻璃边上拐了一下）。
const BORDER_CELLS: usize = 6;
/// 边界格的速度衰减。
const BORDER_DAMPING: f32 = 0.86;
/// 内部的衰减：一次步进剩下这么多速度（0.984 大约 5~8 秒收敛，太久就一直有帧）。
const DAMPING: f32 = 0.984;
/// 波速平方（显式差分下 < 0.5 才稳定）。
const SPEED_SQ: f32 = 0.22;
/// 静止态字阵的基础不透明度（0..255）。
const IDLE_ALPHA: f32 = 46.;
/// 一次步进按 60Hz 算：帧率低的时候多走几步，别让波纹看起来在慢放。
const STEP_SECONDS: f32 = 1. / 60.;

/// 活跃矩形（含端点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActiveRect {
    c0: usize,
    r0: usize,
    c1: usize,
    r1: usize,
}

impl ActiveRect {
    fn contains(&self, c: usize, r: usize) -> bool {
        c >= self.c0 && c <= self.c1 && r >= self.r0 && r <= self.r1
    }
}

/// 首页字阵的波动场。
pub(crate) struct RippleField {
    cols: usize,
    rows: usize,
    height: Vec<f32>,
    velocity: Vec<f32>,
    speed_sq: Vec<f32>,
    active: Option<ActiveRect>,
    /// RGBA8 单元格数据（`cols * rows * 4` 字节）
    pixels: Vec<u8>,
    /// 像素版本号：变了上层才需要重传显存
    revision: u64,
    /// 字形轮换相位（活跃时每 500ms 加一）
    phase: u32,
    /// 上次步进的时刻
    last_step: Option<std::time::Instant>,
    /// 波纹刚收敛：下一帧要把整屏重铺成静止态（否则会停在上一次画的波峰上）
    settle_pending: bool,
}

/// 打包时用到的、每帧都可能变的参数（颜色在 shader 里，这里只有强度）。
pub(crate) struct PackParams {
    /// 波面抬升的增益：越大，波峰的字越亮。
    pub wave_gain: f32,
    /// 云雾/呼吸的强度（0 = 完全均匀）。
    pub cloud: f32,
    /// 指针位置（逻辑像素，相对字阵左上）与光晕半径；`None` 表示没有指针。
    pub aura: Option<(f32, f32, f32)>,
}

impl RippleField {
    pub(crate) fn new() -> Self {
        Self {
            cols: 0,
            rows: 0,
            height: Vec::new(),
            velocity: Vec::new(),
            speed_sq: Vec::new(),
            active: None,
            pixels: Vec::new(),
            revision: 0,
            phase: 0,
            last_step: None,
            settle_pending: false,
        }
    }

    pub(crate) fn cols(&self) -> usize {
        self.cols
    }

    pub(crate) fn rows(&self) -> usize {
        self.rows
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// 还有没有能量（`false` = 可以停帧，画面上就是静止的字阵）。
    pub(crate) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// 按新的逻辑尺寸重建网格。尺寸没变时是空操作（不会清掉正在跑的波纹）。
    pub(crate) fn resize(&mut self, width: f32, height: f32) {
        let cols = ((width / CELL_W).ceil() as usize).max(1);
        let rows = ((height / CELL_H).ceil() as usize).max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        let cells = cols * rows;
        self.height = vec![0.; cells];
        self.velocity = vec![0.; cells];
        self.speed_sq = build_speed_field(cols, rows);
        self.pixels = vec![0; cells * 4];
        self.active = None;
        self.phase = 0;
        self.last_step = None;
        self.fill_idle(&PackParams {
            wave_gain: 1.,
            cloud: 0.,
            aura: None,
        });
    }

    /// 整屏铺一遍静止态（云雾 + 字形），只在重建和静止时用。
    fn fill_idle(&mut self, params: &PackParams) {
        for r in 0..self.rows {
            for c in 0..self.cols {
                self.write_cell(c, r, 0., 0., 0., params);
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// 指针扫过：给线段经过的格子注入一点速度。
    pub(crate) fn wake(&mut self, from: (f32, f32), to: (f32, f32), strength: f32) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }
        let (c0, r0) = self.cell_at(from.0, from.1);
        let (c1, r1) = self.cell_at(to.0, to.1);
        let steps = (c1 as isize - c0 as isize).abs().max((r1 as isize - r0 as isize).abs()) as usize;
        let steps = steps.max(1);
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            let c = (c0 as f32 + (c1 as f32 - c0 as f32) * t).round() as isize;
            let r = (r0 as f32 + (r1 as f32 - r0 as f32) * t).round() as isize;
            self.add_velocity(c, r, -strength);
            // 顺着方向给一点横向推力，波纹才会跟着鼠标走而不是原地抖
            if c1 > c0 {
                self.add_velocity(c - 1, r, -strength * 0.35);
            } else if c1 < c0 {
                self.add_velocity(c + 1, r, -strength * 0.35);
            }
        }
        self.touch(c0, r0);
        self.touch(c1, r1);
    }

    /// 点击：一个圆形波包。
    pub(crate) fn splash(&mut self, x: f32, y: f32, strength: f32) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }
        let (cc, cr) = self.cell_at(x, y);
        let radius = 3i64;
        for dr in -radius..=radius {
            for dc in -radius..=radius {
                let distance = ((dc * dc + dr * dr) as f32).sqrt();
                if distance > radius as f32 {
                    continue;
                }
                let falloff = 1. - distance / radius as f32;
                self.add_velocity(cc + dc, cr + dr, -strength * falloff);
            }
        }
        self.touch(cc, cr);
    }

    /// 走一步。传 `now` 而不是固定步长：掉帧时补步，波纹速度看起来才一致。
    /// 返回这一帧之后是否还有能量。
    pub(crate) fn step(&mut self, now: std::time::Instant) -> bool {
        let Some(active) = self.active else {
            self.last_step = Some(now);
            return false;
        };

        let elapsed = self
            .last_step
            .map(|last| now.saturating_duration_since(last).as_secs_f32())
            .unwrap_or(STEP_SECONDS);
        self.last_step = Some(now);
        let steps = ((elapsed / STEP_SECONDS).round() as usize).clamp(1, 3);

        // 字形轮换跟着走：只在有波的时候换，静止了字阵就定住（0fps 的前提）
        self.phase = self.phase.wrapping_add(steps as u32);

        let mut next = Some(active);
        for _ in 0..steps {
            next = next.and_then(|rect| self.step_once(rect));
        }
        if next.is_none() {
            // 收敛了：下一帧 pack 会把整屏铺回静止态，然后上层停帧
            self.settle_pending = true;
        }
        self.active = next;
        next.is_some()
    }

    /// 一次显式差分步进，只算活跃矩形（外扩一格）。没有能量了就返回 `None`。
    fn step_once(&mut self, active: ActiveRect) -> Option<ActiveRect> {
        let cols = self.cols;
        let rows = self.rows;
        let c0 = active.c0.saturating_sub(1);
        let r0 = active.r0.saturating_sub(1);
        let c1 = (active.c1 + 1).min(cols - 1);
        let r1 = (active.r1 + 1).min(rows - 1);

        for r in r0..=r1 {
            for c in c0..=c1 {
                let i = r * cols + c;
                if !active.contains(c, r) {
                    // 新扩进来的格子这一帧才开始有能量，先给零速度
                    self.velocity[i] = 0.;
                }
                let left = if c > 0 { self.height[i - 1] } else { self.height[i] };
                let right = if c + 1 < cols {
                    self.height[i + 1]
                } else {
                    self.height[i]
                };
                let up = if r > 0 { self.height[i - cols] } else { self.height[i] };
                let down = if r + 1 < rows {
                    self.height[i + cols]
                } else {
                    self.height[i]
                };
                let laplacian = left + right + up + down - 4. * self.height[i];
                let damping = if c < BORDER_CELLS
                    || c + BORDER_CELLS >= cols
                    || r < BORDER_CELLS
                    || r + BORDER_CELLS >= rows
                {
                    DAMPING * BORDER_DAMPING
                } else {
                    DAMPING
                };
                let mut velocity = (self.velocity[i] + laplacian * self.speed_sq[i]) * damping;
                if !velocity.is_finite() {
                    velocity = 0.;
                }
                self.velocity[i] = velocity;
                let mut height = self.height[i] + velocity;
                if height.abs() < REST_HEIGHT && velocity.abs() < REST_HEIGHT {
                    height = 0.;
                    velocity = 0.;
                    self.velocity[i] = 0.;
                }
                self.height[i] = height;
            }
        }

        // 重新算活跃矩形：只扫刚才那块，取有能量的包围盒
        let mut bounds: Option<ActiveRect> = None;
        for r in r0..=r1 {
            for c in c0..=c1 {
                let i = r * cols + c;
                if self.height[i].abs() >= REST_HEIGHT || self.velocity[i].abs() >= REST_HEIGHT {
                    bounds = Some(match bounds {
                        None => ActiveRect { c0: c, r0: r, c1: c, r1: r },
                        Some(b) => ActiveRect {
                            c0: b.c0.min(c),
                            r0: b.r0.min(r),
                            c1: b.c1.max(c),
                            r1: b.r1.max(r),
                        },
                    });
                } else {
                    self.height[i] = 0.;
                    self.velocity[i] = 0.;
                }
            }
        }
        bounds
    }

    /// 把活跃矩形（外扩两格，留给色散）打包进像素缓冲。返回是否变了。
    pub(crate) fn pack(&mut self, params: &PackParams) -> bool {
        let Some(active) = self.active else {
            // 刚收敛：整屏重铺回静止态，让画面停在干净的字阵上（之后就真的不再画了）
            if self.settle_pending {
                self.settle_pending = false;
                self.fill_idle(params);
                return true;
            }
            return false;
        };
        let cols = self.cols;
        let rows = self.rows;
        let c0 = active.c0.saturating_sub(2);
        let r0 = active.r0.saturating_sub(2);
        let c1 = (active.c1 + 2).min(cols - 1);
        let r1 = (active.r1 + 2).min(rows - 1);

        for r in r0..=r1 {
            for c in c0..=c1 {
                let i = r * cols + c;
                let height = self.height[i];
                // 波面梯度：决定色相和棱镜强度
                let left = if c > 0 { self.height[i - 1] } else { height };
                let right = if c + 1 < cols { self.height[i + 1] } else { height };
                let up = if r > 0 { self.height[i - cols] } else { height };
                let down = if r + 1 < rows { self.height[i + cols] } else { height };
                let gx = (right - left) * 0.5;
                let gy = (down - up) * 0.5;
                self.write_cell(c, r, height, gx, gy, params);
            }
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// 写一格像素。
    fn write_cell(&mut self, c: usize, r: usize, height: f32, gx: f32, gy: f32, params: &PackParams) {
        let cols = self.cols;
        let i = (r * cols + c) * 4;
        if i + 3 >= self.pixels.len() {
            return;
        }

        // 静止态的字形：靠坐标散列挑一个，加相位后轮换
        let glyph = {
            let hash = hash_cell(c as u32, r as u32);
            ((hash.wrapping_add(self.phase)) % GLYPH_COUNT as u32) as u8
        };

        let energy = height.abs();
        // 云雾 + 呼吸：同一块区域慢速起伏，静止时就靠它显得有生气
        let cloud = if params.cloud <= 0.001 {
            1.
        } else {
            let x = c as f32 * CELL_W * 0.021;
            let y = r as f32 * CELL_H * 0.037;
            let noise = value_noise(x, y);
            1. - params.cloud * (1. - (0.55 + 0.45 * noise))
        };

        let mut alpha = (IDLE_ALPHA + energy * 1400. * params.wave_gain) * cloud;
        if let Some((ax, ay, radius)) = params.aura {
            let dx = c as f32 * CELL_W + CELL_W * 0.5 - ax;
            let dy = r as f32 * CELL_H + CELL_H * 0.5 - ay;
            let distance = (dx * dx + dy * dy).sqrt();
            let aura = (1. - distance / radius.max(1.)).max(0.);
            alpha += aura * aura * 150.;
        }
        let alpha = alpha.clamp(0., 255.) as u8;

        let mix = (energy * 520. * params.wave_gain).clamp(0., 1.);
        let mix_byte = (mix * 255.) as u8;

        // A 通道：高 3 bit 棱镜强度，低 5 bit 色相
        let gradient = (gx * gx + gy * gy).sqrt();
        let prism = ((gradient * 2600.).clamp(0., 1.) * 7.).round() as u8;
        let angle = gy.atan2(gx);
        let hue = (((angle / TAU) + 0.5).rem_euclid(1.) * 32.).floor() as u8;
        let packed = (prism.min(7) << 5) | hue.min(31);

        self.pixels[i] = glyph;
        self.pixels[i + 1] = alpha;
        self.pixels[i + 2] = mix_byte;
        self.pixels[i + 3] = packed;
    }

    fn cell_at(&self, x: f32, y: f32) -> (isize, isize) {
        (
            (x / CELL_W).floor() as isize,
            (y / CELL_H).floor() as isize,
        )
    }

    fn add_velocity(&mut self, c: isize, r: isize, delta: f32) {
        if c < 0 || r < 0 || c as usize >= self.cols || r as usize >= self.rows {
            return;
        }
        let i = r as usize * self.cols + c as usize;
        let mut velocity = self.velocity[i] + delta;
        if !velocity.is_finite() {
            velocity = 0.;
        }
        self.velocity[i] = velocity.clamp(-2.5, 2.5);
    }

    fn touch(&mut self, c: isize, r: isize) {
        if c < 0 || r < 0 || c as usize >= self.cols || r as usize >= self.rows {
            return;
        }
        let (c, r) = (c as usize, r as usize);
        self.active = Some(match self.active {
            None => ActiveRect { c0: c, r0: r, c1: c, r1: r },
            Some(b) => ActiveRect {
                c0: b.c0.min(c),
                r0: b.r0.min(r),
                c1: b.c1.max(c),
                r1: b.r1.max(r),
            },
        });
    }
}

/// 分形速度场：低频几层噪声叠起来，波在屏幕上的传播速度略有差异。
fn build_speed_field(cols: usize, rows: usize) -> Vec<f32> {
    let mut field = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            let x = c as f32 * 0.06;
            let y = r as f32 * 0.11;
            let mut noise = 0.;
            let mut amp = 0.5;
            let mut freq = 1.;
            for _ in 0..3 {
                noise += amp * value_noise(x * freq, y * freq);
                freq *= 2.;
                amp *= 0.5;
            }
            field.push(SPEED_SQ * (0.7 + 0.6 * noise).clamp(0.3, 1.4));
        }
    }
    field
}

fn hash_cell(c: u32, r: u32) -> u32 {
    let mut hash = c.wrapping_mul(0x9E37_79B9) ^ r.wrapping_mul(0x85EB_CA6B);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x2545_F491);
    hash ^ (hash >> 13)
}

/// 便宜的值噪声：够用来做云雾和速度场，不值得为此引依赖。
fn value_noise(x: f32, y: f32) -> f32 {
    fn hash(x: i32, y: i32) -> f32 {
        let n = (x as u32).wrapping_mul(0x27D4_EB2D) ^ (y as u32).wrapping_mul(0x1656_67B1);
        let n = (n ^ (n >> 15)).wrapping_mul(0x2545_F491);
        (n ^ (n >> 13)) as f32 / u32::MAX as f32
    }
    let xi = x.floor();
    let yi = y.floor();
    let xf = x - xi;
    let yf = y - yi;
    let u = xf * xf * (3. - 2. * xf);
    let v = yf * yf * (3. - 2. * yf);
    let (xi, yi) = (xi as i32, yi as i32);
    let a = hash(xi, yi);
    let b = hash(xi + 1, yi);
    let c = hash(xi, yi + 1);
    let d = hash(xi + 1, yi + 1);
    let top = a + (b - a) * u;
    let bottom = c + (d - c) * u;
    top + (bottom - top) * v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn params() -> PackParams {
        PackParams {
            wave_gain: 1.,
            cloud: 0.3,
            aura: None,
        }
    }

    #[test]
    fn resize_builds_a_grid_and_fills_the_idle_pattern() {
        let mut field = RippleField::new();
        field.resize(700., 450.);
        assert_eq!(field.cols(), 100);
        assert_eq!(field.rows(), 30);
        assert_eq!(field.pixels().len(), 100 * 30 * 4);
        assert!(!field.is_active());
        // 静止态也要有字形和一点点亮度，否则首页是一整块空白
        assert!(field.pixels().chunks_exact(4).all(|cell| cell[0] < 62));
        assert!(field.pixels().chunks_exact(4).any(|cell| cell[1] > 0));
    }

    #[test]
    fn resizing_to_the_same_size_keeps_the_running_wave() {
        let mut field = RippleField::new();
        field.resize(700., 450.);
        field.splash(350., 200., 1.2);
        assert!(field.is_active());
        let revision = field.revision();
        field.resize(700., 450.);
        assert!(field.is_active());
        assert_eq!(field.revision(), revision, "尺寸没变就该原样保留");
    }

    #[test]
    fn splash_spreads_then_settles_and_reports_inactive() {
        let mut field = RippleField::new();
        field.resize(700., 450.);
        field.splash(350., 200., 1.5);
        assert!(field.is_active());
        let start = Instant::now();
        let mut frames = 0;
        while field.is_active() && frames < 2000 {
            field.step(start + Duration::from_millis(16 * frames as u64));
            frames += 1;
        }
        assert!(!field.is_active(), "波纹应当收敛回静止");
        assert!(frames > 5, "不该一两帧就没了：{frames}");
        assert!(frames < 2000, "更不该一直抖：{frames}");
    }

    #[test]
    fn stepping_a_resting_field_is_a_no_op() {
        let mut field = RippleField::new();
        field.resize(700., 450.);
        let revision = field.revision();
        let start = Instant::now();
        for frame in 0..10 {
            assert!(!field.step(start + Duration::from_millis(16 * frame)));
        }
        // 没有波就不该打包，像素版本号原地不动（上层据此不重传显存）
        assert!(!field.pack(&params()));
        assert_eq!(field.revision(), revision);
    }

    #[test]
    fn pack_writes_energy_into_the_active_cells_only() {
        let mut field = RippleField::new();
        field.resize(700., 450.);
        let before = field.revision();
        field.splash(350., 200., 1.5);
        let start = Instant::now();
        field.step(start);
        assert!(field.pack(&params()));
        assert_eq!(field.revision(), before + 1);
        // 波心那一格的亮度要明显高于远处的静止态
        let cols = field.cols();
        let center = ((200. / CELL_H) as usize * cols + (350. / CELL_W) as usize) * 4;
        let far = (2 * cols + 2) * 4;
        assert!(
            field.pixels()[center + 1] > field.pixels()[far + 1],
            "波心的字阵应当更亮"
        );
        // 每格的 A 通道必须落在「高 3 bit + 低 5 bit」的范围里
        assert!(field.pixels().chunks_exact(4).all(|cell| cell[3] <= 0xFF));
    }

    #[test]
    fn aura_lifts_the_cells_under_the_pointer() {
        let mut field = RippleField::new();
        field.resize(700., 450.);
        field.splash(350., 200., 1.5);
        field.step(Instant::now());
        let dark = PackParams {
            wave_gain: 1.,
            cloud: 0.,
            aura: None,
        };
        let lit = PackParams {
            wave_gain: 1.,
            cloud: 0.,
            aura: Some((350., 200., 200.)),
        };
        field.pack(&dark);
        let cols = field.cols();
        let center = ((200. / CELL_H) as usize * cols + (350. / CELL_W) as usize) * 4 + 1;
        let without = field.pixels()[center];
        field.pack(&lit);
        let with = field.pixels()[center];
        assert!(with >= without, "指针光晕不该让字变暗：{without} → {with}");
    }
}
