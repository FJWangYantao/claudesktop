//! 发送前的图片处理（见「设计说明-聊天与Code分离.md」第七章）。
//!
//! 目标（两家接口都满足）：
//! - **转正**：手机照片的 EXIF 方向标记先应用，模型看到的和用户看到的一样正；
//! - **缩小**：长边 ≤ 1568 px 且总像素 ≤ 1.15 MP（Anthropic 的建议值，OpenAI 也在这个量级）；
//! - **重编码**：顺带去掉全部元数据（含 GPS）；截图 / 带透明 → PNG，照片 → JPEG 85；
//!   BMP / TIFF → PNG（两家接口都不收 BMP）；
//! - **兜底**：处理完单张仍超过 3.75 MB（base64 后 5 MB）就降质量 / 再缩一档。
//!
//! 这一层不依赖 UI、不依赖内核，纯函数 + 单测（测试图在代码里现生成）。

use std::io::Cursor;

/// 长边上限（像素）：Anthropic 建议 1568 px 以内，更大的图服务端也会先缩
pub const MAX_EDGE: u32 = 1568;
/// 总像素上限（约 1.15 MP）
pub const MAX_PIXELS: u64 = 1_150_000;
/// 接口硬上限（base64 后 5 MB，这里按原始字节留余量）
pub const MAX_BYTES: usize = 3_750_000;
/// 重新编码时 JPEG 的质量
const JPEG_QUALITY: u8 = 85;

/// 处理后的图片：模型实际看到的那份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// `image/png` / `image/jpeg`
    pub media_type: String,
    /// 编码后的字节
    pub bytes: Vec<u8>,
    /// 处理后的宽高（悬停提示「1568×1176 · 约 2.5k token」用）
    pub width: u32,
    pub height: u32,
}

impl Prepared {
    /// 估算 token（Anthropic 的算法：宽高各除 750 取整后再乘，误差不大，只作提示）。
    pub fn approx_tokens(&self) -> u64 {
        let w = (self.width as f64 / 750.).ceil().max(1.);
        let h = (self.height as f64 / 750.).ceil().max(1.);
        (w * h) as u64 * 1_000
    }

    /// 按接口口径的 base64 后大小（估算：4/3 倍加余量）。
    pub fn base64_len(&self) -> usize {
        self.bytes.len().div_ceil(3) * 4
    }
}

/// 处理失败的原因（都是给用户看的中文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepError {
    /// 这种格式读不了（HEIC、AVIF、RAW……）
    Unsupported(String),
    /// 解不开（文件坏了、不是图片）
    Undecodable(String),
    /// 想留下来的是动图，但太大了
    AnimationTooLarge,
    /// 怎么缩都放不进上限
    StillTooLarge(usize),
}

impl PrepError {
    /// 界面里 toast 的那句话。
    pub fn message(&self) -> String {
        match self {
            Self::Unsupported(ext) => format!("这种格式（{ext}）读不了，先另存为 JPG 或 PNG"),
            Self::Undecodable(why) => format!("这张图读不了：{why}"),
            Self::AnimationTooLarge => {
                "这张动图太大，聊天里发不出去（会缩成第一帧吗？不会——请先压小）".into()
            }
            Self::StillTooLarge(bytes) => {
                format!("这张图缩到最小还有 {}，超过了接口的单张上限", human_size(*bytes))
            }
        }
    }
}

impl std::fmt::Display for PrepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for PrepError {}

/// 处理的详细程度（设置里「图片清晰度」，见设计说明 7.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// 标准：长边 1568 px、约 1.15 MP（默认）
    #[default]
    Standard,
    /// 原图：只在超过接口硬上限时才缩
    Original,
}

/// 把一张图片处理成可以发出去的那份。
///
/// `name` 只用来认扩展名（HEIC / AVIF / RAW 这类在解码前就挡掉，报错能说清格式）。
pub fn prepare(name: &str, bytes: &[u8], quality: Quality) -> Result<Prepared, PrepError> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    // 解码器认不出的格式先挡掉：报错里能带出扩展名，比「解码失败」有用
    if matches!(ext.as_str(), "heic" | "heif" | "avif" | "raw" | "cr2" | "nef" | "arw" | "dng") {
        return Err(PrepError::Unsupported(ext.to_ascii_uppercase()));
    }

    // 解码 + 读 EXIF 方向（同一个解码器上先问方向，再读像素）
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| PrepError::Undecodable(e.to_string()))?;
    let format = reader.format();
    let mut decoder = reader.into_decoder().map_err(|e| PrepError::Undecodable(e.to_string()))?;
    // 动图：GIF 在上限内原样发（保住动画），超了取第一帧
    let animated = matches!(format, Some(image::ImageFormat::Gif));
    let orientation = image::ImageDecoder::orientation(&mut decoder)
        .map_err(|e| PrepError::Undecodable(e.to_string()))?;
    let mut image = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| PrepError::Undecodable(e.to_string()))?;
    if orientation != image::metadata::Orientation::NoTransforms {
        image.apply_orientation(orientation);
    }

    // 小动图原样（保住动画）；其余一律重新编码，顺带把元数据（含 GPS）全去掉
    if animated
        && bytes.len() <= MAX_BYTES
        && fits_limits(image.width(), image.height(), Quality::Original)
    {
        return Ok(Prepared {
            media_type: "image/gif".into(),
            bytes: bytes.to_vec(),
            width: image.width(),
            height: image.height(),
        });
    }
    if animated && !fits_limits(image.width(), image.height(), Quality::Standard) {
        // 超上限的动图：取第一帧继续走下面的流程
    }

    // 缩小到上限内
    let image = shrink(image, quality);
    // 截图 / 公式 / 图表 / 带透明：PNG（无损，字不糊）；照片：JPEG 85
    let keep_alpha = has_alpha(&image);
    let mut prepared = encode(image, keep_alpha, JPEG_QUALITY)?;
    // 兜底：单张仍超过硬上限就降质量 / 再缩一档
    let mut edge = prepared.width.max(prepared.height);
    let mut quality_level = JPEG_QUALITY;
    while prepared.base64_len() > 5_000_000 || prepared.bytes.len() > MAX_BYTES {
        if quality_level > 60 {
            quality_level -= 15;
            let img = decode_again(&prepared.bytes)?;
            prepared = encode(img, false, quality_level)?;
            continue;
        }
        edge = (edge as f64 * 0.8) as u32;
        if edge < 256 {
            return Err(PrepError::StillTooLarge(prepared.bytes.len()));
        }
        let img = decode_again(&prepared.bytes)?;
        let img = img.resize(edge, edge, image::imageops::FilterType::Lanczos3);
        prepared = encode(img, keep_alpha, quality_level)?;
    }
    Ok(prepared)
}

/// 这两个宽高在给定清晰度下要不要缩。
fn fits_limits(w: u32, h: u32, quality: Quality) -> bool {
    match quality {
        Quality::Original => {
            // 原图只看接口硬上限（长边 8000 px、5 MB 是 Anthropic 的硬线）
            w.max(h) <= 8_000
        }
        Quality::Standard => w.max(h) <= MAX_EDGE && u64::from(w) * u64::from(h) <= MAX_PIXELS,
    }
}

/// 缩到上限内（等比，只缩不放）。
fn shrink(image: image::DynamicImage, quality: Quality) -> image::DynamicImage {
    let (w, h) = (image.width(), image.height());
    if fits_limits(w, h, quality) {
        return image;
    }
    let (target_w, target_h) = match quality {
        Quality::Standard => {
            // 长边到 MAX_EDGE，再看总像素：两个都满足
            let by_edge = MAX_EDGE as f64 / f64::from(w.max(h));
            let by_pixels = (MAX_PIXELS as f64 / (f64::from(w) * f64::from(h))).sqrt();
            let scale = by_edge.min(by_pixels);
            ((f64::from(w) * scale) as u32, (f64::from(h) * scale) as u32)
        }
        Quality::Original => {
            let scale = 8_000_f64 / f64::from(w.max(h));
            ((f64::from(w) * scale) as u32, (f64::from(h) * scale) as u32)
        }
    };
    image.resize_exact(target_w.max(1), target_h.max(1), image::imageops::FilterType::Lanczos3)
}

/// 有没有透明像素（有就编 PNG：JPEG 会把透明压成黑色）。
fn has_alpha(image: &image::DynamicImage) -> bool {
    matches!(
        image.color(),
        image::ColorType::La8
            | image::ColorType::La16
            | image::ColorType::Rgba8
            | image::ColorType::Rgba16
            | image::ColorType::Rgba32F
    )
}

/// 按「有没有透明」选编码：PNG 或 JPEG。
fn encode(image: image::DynamicImage, png: bool, quality: u8) -> Result<Prepared, PrepError> {
    let width = image.width();
    let height = image.height();
    let mut out = Cursor::new(Vec::new());
    if png {
        image
            .write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| PrepError::Undecodable(e.to_string()))?;
        Ok(Prepared { media_type: "image/png".into(), bytes: out.into_inner(), width, height })
    } else {
        let rgb = image.to_rgb8();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        encoder
            .encode(rgb.as_raw(), width, height, image::ExtendedColorType::Rgb8)
            .map_err(|e| PrepError::Undecodable(e.to_string()))?;
        Ok(Prepared { media_type: "image/jpeg".into(), bytes: out.into_inner(), width, height })
    }
}

fn decode_again(bytes: &[u8]) -> Result<image::DynamicImage, PrepError> {
    image::load_from_memory(bytes).map_err(|e| PrepError::Undecodable(e.to_string()))
}

/// 给人看的大小。
pub fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

// ---------- 模型能不能看图（第七章 7.3） ----------

/// 这个模型能不能看图片。`None` = 不知道，照发。
///
/// 端点报了 `input_modalities` 就用它；没报就查能力表（按「协议 + 模型名」，
/// 和思考档位一个写法）。表里也没有就是不知道。
pub fn vision_support(protocol: &str, model: &str) -> Option<bool> {
    let m = model.to_ascii_lowercase();
    // 明确只看文字的：DeepSeek 的对话 / 推理型号、GLM 的文本型号、Kimi 的老型号
    const TEXT_ONLY: &[&str] = &[
        "deepseek-chat",
        "deepseek-reasoner",
        "glm-4-flash",
        "glm-4.5",
        "glm-4.6",
        "kimi-k2",
        "qwen-turbo",
    ];
    if TEXT_ONLY.iter().any(|name| m == *name || m.starts_with(&format!("{name}-"))) {
        return Some(false);
    }
    // 明确能看的
    const VISION: &[&str] = &[
        "gpt-4o",
        "gpt-4.1",
        "gpt-5",
        "claude-3",
        "claude-4",
        "claude-sonnet",
        "claude-opus",
        "claude-haiku",
        "gemini",
        "glm-4.5v",
        "glm-4v",
        "qwen-vl",
        "qwen2-vl",
        "qwen2.5-vl",
        "internvl",
        "llava",
        "pixtral",
        "minicpm-v",
        "step-1v",
        "moonshot-v1-vision",
    ];
    if VISION.iter().any(|name| m.contains(name)) {
        return Some(true);
    }
    let _ = protocol;
    None
}

/// 从模型列表条目的 `architecture.input_modalities` 里读「能不能看图」。
/// （OpenRouter 这类网关会报；没报返回 None。）
pub fn vision_from_modalities(entry: &serde_json::Value) -> Option<bool> {
    let list = entry
        .get("architecture")
        .and_then(|a| a.get("input_modalities"))
        .or_else(|| entry.get("input_modalities"))
        .or_else(|| entry.get("modalities"))?;
    let list = list.as_array()?;
    Some(list.iter().any(|m| {
        m.as_str().is_some_and(|s| s.eq_ignore_ascii_case("image"))
            || m.get("type").and_then(|t| t.as_str()) == Some("image")
    }))
}

/// 端点回的 400 是不是「这个模型看不了图片」（第七章 7.3 的识别与翻译）。
pub fn image_rejection(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    const HINTS: &[&str] = &[
        "image_url is not supported",
        "does not support image",
        "does not support images",
        "image input is not supported",
        "not support vision",
        "no vision",
        "multimodal",
        "invalid content type",
        "image not supported",
        "unsupported image",
        "不支持图片",
    ];
    HINTS.iter().any(|h| m.contains(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 现生成一张 PNG（纯色，可带透明）。
    fn png_bytes(w: u32, h: u32, alpha: bool) -> Vec<u8> {
        let dyn_img = if alpha {
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(w, h, |x, _| {
                image::Rgba([(x % 255) as u8, 40, 90, 128])
            }))
        } else {
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
                image::Rgb([(x % 255) as u8, (y % 255) as u8, 90])
            }))
        };
        let mut out = Cursor::new(Vec::new());
        dyn_img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn big_photos_are_shrunk_to_the_suggested_size() {
        // 4000×3000（1200 万像素）缩到 1568 px 上限之内
        let bytes = png_bytes(4000, 3000, false);
        let out = prepare("photo.png", &bytes, Quality::Standard).unwrap();
        assert!(out.width.max(out.height) <= MAX_EDGE, "{}x{}", out.width, out.height);
        assert!(
            u64::from(out.width) * u64::from(out.height) <= MAX_PIXELS,
            "{}x{}",
            out.width,
            out.height
        );
        // 宽高比没变
        let ratio_in = 4000. / 3000.;
        let ratio_out = f64::from(out.width) / f64::from(out.height);
        assert!((ratio_in - ratio_out).abs() < 0.01, "{ratio_out}");
    }

    #[test]
    fn small_photos_are_reencoded_without_metadata() {
        let bytes = png_bytes(200, 150, false);
        let out = prepare("small.png", &bytes, Quality::Standard).unwrap();
        assert_eq!((out.width, out.height), (200, 150));
        // 不透明 → JPEG，元数据全没了（重新编码）
        assert_eq!(out.media_type, "image/jpeg");
        assert!(!out.bytes.is_empty());
    }

    #[test]
    fn transparency_stays_png() {
        let bytes = png_bytes(100, 100, true);
        let out = prepare("logo.png", &bytes, Quality::Standard).unwrap();
        assert_eq!(out.media_type, "image/png");
        // 解回来还是带透明的
        let back = image::load_from_memory(&out.bytes).unwrap();
        assert!(back.color().has_alpha());
    }

    #[test]
    fn bmp_becomes_png_or_jpeg() {
        // BMP 两家接口都不收：进来就重新编码
        let img =
            image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([(x * 3) as u8, (y * 3) as u8, 10]));
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Bmp).unwrap();
        let prepared = prepare("shot.bmp", &out.into_inner(), Quality::Standard).unwrap();
        assert!(matches!(prepared.media_type.as_str(), "image/png" | "image/jpeg"));
    }

    #[test]
    fn heic_and_raw_are_refused_with_the_format_named() {
        let err = prepare("IMG_1234.HEIC", b"whatever", Quality::Standard).unwrap_err();
        assert!(matches!(err, PrepError::Unsupported(_)), "{err:?}");
        assert!(err.message().contains("HEIC"), "{}", err.message());
        assert!(prepare("x.cr2", b"x", Quality::Standard).is_err());
    }

    #[test]
    fn broken_files_are_reported_not_panicked() {
        let err = prepare("x.png", b"not an image", Quality::Standard).unwrap_err();
        assert!(matches!(err, PrepError::Undecodable(_)), "{err:?}");
    }

    #[test]
    fn original_quality_keeps_large_but_bounded_images() {
        // 2000 px：标准会缩到上限内，原图不缩（还在 8000 px 硬上限内）
        let bytes = png_bytes(2000, 1200, false);
        let standard = prepare("a.png", &bytes, Quality::Standard).unwrap();
        assert!(standard.width.max(standard.height) <= MAX_EDGE);
        assert!(u64::from(standard.width) * u64::from(standard.height) <= MAX_PIXELS);
        let original = prepare("a.png", &bytes, Quality::Original).unwrap();
        assert_eq!((original.width, original.height), (2000, 1200));
    }

    #[test]
    fn gif_within_limits_keeps_its_animation() {
        // 一张小 GIF：原样发（保住动画）
        let img = image::RgbImage::from_fn(32, 32, |x, y| image::Rgb([x as u8, y as u8, 0]));
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Gif).unwrap();
        let gif = out.into_inner();
        let prepared = prepare("a.gif", &gif, Quality::Standard).unwrap();
        assert_eq!(prepared.media_type, "image/gif");
        assert_eq!(prepared.bytes, gif);
    }

    #[test]
    fn token_estimate_and_size_helper() {
        let p = Prepared {
            media_type: "image/jpeg".into(),
            bytes: vec![0; 1000],
            width: 1568,
            height: 1176,
        };
        // 1568/750 = 2.09 → 3；1176/750 = 1.57 → 2；3 × 2 × 1000 = 6000
        assert_eq!(p.approx_tokens(), 6_000);
        assert_eq!(p.base64_len(), 1336);
        assert_eq!(human_size(812), "812 B");
        assert_eq!(human_size(12_000), "12 KB");
        assert_eq!(human_size(3_000_000), "2.9 MB");
    }

    #[test]
    fn vision_table_and_rejection_hints() {
        assert_eq!(vision_support("openai", "deepseek-chat"), Some(false));
        assert_eq!(vision_support("openai", "gpt-4o-mini"), Some(true));
        assert_eq!(vision_support("anthropic", "claude-sonnet-4-5"), Some(true));
        assert_eq!(vision_support("openai", "some-unknown-model"), None);
        // 网关报的 input_modalities
        let entry = serde_json::json!({
            "id": "google/gemini-2.5-pro",
            "architecture": { "input_modalities": ["text", "image"] }
        });
        assert_eq!(vision_from_modalities(&entry), Some(true));
        let text_only = serde_json::json!({ "input_modalities": ["text"] });
        assert_eq!(vision_from_modalities(&text_only), Some(false));
        assert_eq!(vision_from_modalities(&serde_json::json!({})), None);
        // 400 的识别
        assert!(image_rejection("image_url is not supported for this model"));
        assert!(image_rejection("This model does not support image inputs."));
        assert!(!image_rejection("rate limit exceeded"));
    }
}
