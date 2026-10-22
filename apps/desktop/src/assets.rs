//! 资源：gpui-kit 自带的全套图标，加上运行时登记进来的公式 SVG
//! （见「设计说明-聊天与Code分离.md」6.3）。
//!
//! 公式的结果是自包含 SVG（字形转路径），排版在后台线程做完就按内容哈希登记到
//! [`MATH_PREFIX`] 下面，画的时候按名字取出来贴进 gpui 的图集缓存。换主题不用重新生成：
//! gpui 画 SVG 是单色蒙版 + 着色，颜色在画的时候给。

use gpui_kit::AssetSource;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 公式 SVG 的路径前缀（`math/<哈希>.svg`）。
pub const MATH_PREFIX: &str = "math/";

/// 按名字登记过的公式 SVG。
fn registry() -> &'static Mutex<HashMap<String, &'static [u8]>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, &'static [u8]>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 登记一份公式 SVG（已经登记过的同名直接跳过）。
///
/// 内存只留一份：同一段公式在整段会话里只占一次（排版缓存命中时哈希也一样）。
pub fn register_math(svg: &str) {
    let name = math_name(svg);
    let mut map = registry().lock().unwrap();
    if !map.contains_key(&name) {
        // 一次性泄漏一小块静态缓冲：资源源要求 'static，生命周期跟着进程走
        //（和 gpui-kit 内嵌的图标一个路子）
        let bytes: &'static [u8] = Box::leak(svg.as_bytes().to_vec().into_boxed_slice());
        map.insert(name, bytes);
    }
}

/// 一份公式 SVG 的资源名。
pub fn math_name(svg: &str) -> String {
    format!("{MATH_PREFIX}{}", crate::app::math::hash_of(svg))
}

/// 资源源：`math/` 前缀查运行时登记表，其余转发给 gpui-kit 内嵌的图标。
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(rest) = path.strip_prefix(MATH_PREFIX) {
            let hit = registry().lock().unwrap().get(rest).copied();
            return Ok(hit.map(Cow::Borrowed));
        }
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<gpui_kit::SharedString>> {
        if path.starts_with(MATH_PREFIX) || path.trim_end_matches('/') == "math" {
            return Ok(registry()
                .lock()
                .unwrap()
                .keys()
                .map(|k| format!("{MATH_PREFIX}{k}").into())
                .collect());
        }
        gpui_kit::assets::AllAssets.list(path)
    }
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
}
