//! 品牌形象「小陶」：上了釉的赤陶小石子（同 exe 图标）。图都在 `assets/brand/`，由 `tools/make-icon.mjs` 生成。
//!
//! - [`brand_mark`]：静态的标志（侧栏品牌位、首页问候语）。
//! - [`Mood`]：运行状态行上会动的表情，一个状态几帧轮着放（琢磨中头顶冒泡、回复时嘴一张一合……）。
//!   画法和别的循环动画一样：内容区里只放占位格，由 `motion::LoopOverlay` 按全局时钟取帧来画（见 `motion::mascot`）。
//!
//! gpui 按 SVG 固有尺寸的 2 倍栅格化，各份 SVG 的固有尺寸就写成它的显示尺寸（20 / 40），缩放少、边缘不糊。

use gpui_kit::*;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

fn svg(bytes: &'static [u8]) -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Svg, bytes.to_vec()))
}

/// 静态的小陶，`size` 是边长。1.5rem 以内用简化版（去掉釉线和高光细节、眼睛放大），大的用细节版。
pub(super) fn brand_mark(size: Rems) -> Img {
    static SMALL: LazyLock<Arc<Image>> =
        LazyLock::new(|| svg(include_bytes!("../../assets/brand/xiaotao-small.svg")));
    static LARGE: LazyLock<Arc<Image>> =
        LazyLock::new(|| svg(include_bytes!("../../assets/brand/xiaotao.svg")));
    let image = if size.0 <= 1.5 { &SMALL } else { &LARGE };
    img(Arc::clone(image)).size(size).flex_shrink_0()
}

/// 运行状态行的表情（对应 `Activity` 和不忙时的两种准备状态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mood {
    /// 琢磨中（思考、调工具）：往右上看，头顶的泡一个个冒出来
    Think,
    /// 正在回复：嘴一张一合
    Talk,
    /// 等待你授权：睁大眼，红色问号上下跳
    Ask,
    /// 自动重试：冒汗，汗珠往下滑
    Sweat,
    /// 压缩上下文：闭眼使劲，压扁再回弹
    Squish,
    /// 续接会话：眼睛慢慢睁开，再眨一下
    Wake,
    /// 准备 worktree：眯着眼左右张望
    Scan,
}

macro_rules! frames {
    ($($file:literal),+ $(,)?) => {{
        static FRAMES: LazyLock<Vec<Arc<Image>>> = LazyLock::new(|| {
            vec![$(svg(include_bytes!(concat!("../../assets/brand/moods/", $file, ".svg")))),+]
        });
        &FRAMES
    }};
}

impl Mood {
    /// 这个表情的各帧（`tools/xiaotao.mjs` 的 `MOODS`，帧号即下标）
    pub(super) fn frames(self) -> &'static [Arc<Image>] {
        match self {
            Mood::Think => frames!("think-0", "think-1", "think-2", "think-3"),
            Mood::Talk => frames!("talk-0", "talk-1", "talk-2"),
            Mood::Ask => frames!("ask-0", "ask-1"),
            Mood::Sweat => frames!("sweat-0", "sweat-1", "sweat-2"),
            Mood::Squish => frames!("squish-0", "squish-1"),
            Mood::Wake => frames!("wake-0", "wake-1", "wake-2"),
            Mood::Scan => frames!("scan-0", "scan-1"),
        }
    }

    /// 播放顺序（帧号）和每一格停多久：一遍放完从头再来
    fn script(self) -> (&'static [usize], Duration) {
        let ms = Duration::from_millis;
        match self {
            // 泡冒满了多停一拍再清空
            Mood::Think => (&[0, 1, 2, 3, 3], ms(380)),
            Mood::Talk => (&[0, 1, 2, 1], ms(200)),
            Mood::Ask => (&[0, 1], ms(500)),
            Mood::Sweat => (&[0, 1, 2], ms(320)),
            Mood::Squish => (&[0, 1], ms(450)),
            // 睁开后停一会儿眨一下（1 号是半睁）
            Mood::Wake => (&[0, 0, 1, 2, 2, 2, 2, 2, 1], ms(300)),
            Mood::Scan => (&[0, 1], ms(600)),
        }
    }

    /// 时钟走到 `elapsed` 时该放哪一帧
    pub(super) fn frame_at(self, elapsed: Duration) -> usize {
        let (order, step) = self.script();
        order[(elapsed.as_millis() / step.as_millis()) as usize % order.len()]
    }

    /// 减少动态效果时停在哪一帧：选最能说明状态的那张
    pub(super) fn rest_frame(self) -> usize {
        match self {
            Mood::Think => 3,
            Mood::Sweat => 1,
            Mood::Wake => 2,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Mood; 7] =
        [Mood::Think, Mood::Talk, Mood::Ask, Mood::Sweat, Mood::Squish, Mood::Wake, Mood::Scan];

    #[core::prelude::v1::test]
    fn scripts_only_use_existing_frames() {
        for mood in ALL {
            let n = mood.frames().len();
            assert!(mood.script().0.iter().all(|&i| i < n), "{mood:?}");
            assert!(mood.rest_frame() < n, "{mood:?}");
        }
    }

    #[core::prelude::v1::test]
    fn frame_follows_the_script_and_loops() {
        let step = Mood::Think.script().1;
        assert_eq!(Mood::Think.frame_at(Duration::ZERO), 0);
        assert_eq!(Mood::Think.frame_at(step * 3), 3);
        // 一遍 5 格放完回到开头
        assert_eq!(Mood::Think.frame_at(step * 5), 0);
    }
}
