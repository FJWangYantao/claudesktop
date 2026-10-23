//! 资源：gpui-kit 自带的全套图标，加上运行时登记进来的公式 SVG 和远程图片
//! （见「设计说明-聊天与Code分离.md」6.3、7.5）。
//!
//! 公式的结果是自包含 SVG（字形转路径），排版在后台线程做完就按内容哈希登记到
//! [`MATH_PREFIX`] 下面，画的时候按名字取出来贴进 gpui 的图集缓存。换主题不用重新生成：
//! gpui 画 SVG 是单色蒙版 + 着色，颜色在画的时候给。
//!
//! 远程图片同理登记在 [`IMAGE_PREFIX`] 下面：字节是按内容哈希存的，同一张图在几条
//! 回复里出现只占一份，也不走 `img()` 的 URI 分支（那条路每帧都可能重试）。

use gpui_kit::AssetSource;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 公式 SVG 的路径前缀（`math/<哈希>.svg`）。
pub const MATH_PREFIX: &str = "math/";
/// 远程图片的路径前缀（`remote/<哈希>.<扩展名>`）。
pub const IMAGE_PREFIX: &str = "remote/";

/// 按名字登记过的资源（公式 SVG、远程图片）。
fn registry() -> &'static Mutex<HashMap<String, &'static [u8]>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, &'static [u8]>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 登记一张远程图片的原始字节，返回交给 `img()` 的资源名。
///
/// 认不出格式返回 `None`（调用方据此报「读不出来」）。名字按内容哈希，
/// 所以同一张图重复登记是同一个名字、只占一份内存。
pub fn register_image(bytes: &[u8]) -> Option<String> {
    let ext = image_extension(image::guess_format(bytes).ok()?)?;
    // 表里按去掉 `remote/` 前缀的名字存（`load` 也是去掉前缀再查），和公式一样
    let key = format!("{}.{ext}", crate::app::remote_image::hash_bytes(bytes));
    let mut map = registry().lock().unwrap();
    // 和公式一样泄漏一小块静态缓冲：资源源要求 'static，生命周期跟着进程走
    map.entry(key.clone()).or_insert_with(|| Box::leak(bytes.to_vec().into_boxed_slice()));
    Some(format!("{IMAGE_PREFIX}{key}"))
}

/// 只收 gpui 画得出来的格式（别的给个 `None`，让界面说「读不出来」而不是崩）。
fn image_extension(format: image::ImageFormat) -> Option<&'static str> {
    Some(match format {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::Gif => "gif",
        image::ImageFormat::WebP => "webp",
        image::ImageFormat::Bmp => "bmp",
        _ => return None,
    })
}

/// 登记一份公式 SVG（已经登记过的同名直接跳过）。
///
/// 内存只留一份：同一段公式在整段会话里只占一次（排版缓存命中时哈希也一样）。
pub fn register_math(svg: &str) {
    // 表里按去掉 `math/` 前缀的名字存（`load` 也是去掉前缀再查）
    let name = crate::app::math::hash_of(svg);
    let mut map = registry().lock().unwrap();
    // 一次性泄漏一小块静态缓冲：资源源要求 'static，生命周期跟着进程走
    //（和 gpui-kit 内嵌的图标一个路子）
    map.entry(name).or_insert_with(|| Box::leak(svg.as_bytes().to_vec().into_boxed_slice()));
}

/// 一份公式 SVG 的资源名。
pub fn math_name(svg: &str) -> String {
    format!("{MATH_PREFIX}{}", crate::app::math::hash_of(svg))
}

/// 资源源：`math/` 前缀查运行时登记表，其余转发给 gpui-kit 内嵌的图标。
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        for prefix in [MATH_PREFIX, IMAGE_PREFIX] {
            if let Some(rest) = path.strip_prefix(prefix) {
                let hit = registry().lock().unwrap().get(rest).copied();
                return Ok(hit.map(Cow::Borrowed));
            }
        }
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<gpui_kit::SharedString>> {
        if path.starts_with(MATH_PREFIX) || path.trim_end_matches('/') == "math" {
            return Ok(registry()
                .lock()
                .unwrap()
                .keys()
                .filter(|k| k.ends_with(".svg"))
                .map(|k| format!("{MATH_PREFIX}{k}").into())
                .collect());
        }
        if path.starts_with(IMAGE_PREFIX) || path.trim_end_matches('/') == "remote" {
            return Ok(registry()
                .lock()
                .unwrap()
                .keys()
                .filter(|k| !k.ends_with(".svg"))
                .map(|k| format!("{IMAGE_PREFIX}{k}").into())
                .collect());
        }
        gpui_kit::assets::AllAssets.list(path)
    }
}

/// 一张远程图片已经取回并登记过的字节（`None` = 还没加载 / 加载失败）。
///
/// 「另存为…」写盘要原始字节；画图用 [`crate::app::remote_image::registered_name`]。
pub fn cached(url: &str) -> Option<Vec<u8>> {
    let name = crate::app::remote_image::registered_name(url)?;
    let key = name.strip_prefix(IMAGE_PREFIX)?;
    registry().lock().unwrap().get(key).map(|bytes| bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_math_svgs_load_by_name() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"><path d=\"M0 0h1\"/></svg>";
        let name = math_name(svg);
        assert!(name.starts_with(MATH_PREFIX));
        // 登记之前取不到（画的时候就是空白，不 panic）
        assert!(Assets.load(&name).unwrap().is_none());
        register_math(svg);
        assert_eq!(&*Assets.load(&name).unwrap().unwrap(), svg.as_bytes());
        // 同一份内容登记一次就够，名字不变
        register_math(svg);
        assert_eq!(name, math_name(svg));
    }

    #[test]
    fn registered_images_load_by_name() {
        // 1×1 的 PNG（gpui 只要能认出格式，登记这一步不解码）
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let name = register_image(png).expect("PNG 认得出来");
        assert!(name.starts_with(IMAGE_PREFIX), "{name}");
        assert_eq!(&*Assets.load(&name).unwrap().unwrap(), png);
        // 列目录给的也是能直接 load 的名字（不重复前缀）
        assert!(Assets.list(IMAGE_PREFIX).unwrap().iter().any(|n| n.as_ref() == name));
        // 同一份字节登记两次是同一个名字（只占一份）
        assert_eq!(register_image(png).as_deref(), Some(name.as_str()));
        // 不是图片的字节给 None，界面据此说「读不出来」
        assert!(register_image(b"<html>nope</html>").is_none());
    }
}
