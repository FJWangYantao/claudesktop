//! 资源：gpui-kit 图标、内嵌内核标志，以及运行时登记的公式 SVG 和远程图片
//! （见「设计说明-聊天与Code分离.md」6.3、7.5）。
//!
//! 公式的结果是自包含 SVG（字形转路径），排版在后台线程做完就按内容哈希登记到
//! [`MATH_PREFIX`] 下面，画的时候按名字取出来贴进 gpui 的图集缓存。换主题不用重新生成：
//! gpui 画 SVG 是单色蒙版 + 着色，颜色在画的时候给。
//!
//! 远程图片同理登记在 [`IMAGE_PREFIX`] 下面：字节是按内容哈希存的，同一张图在几条
//! 回复里出现只占一份，也不走 `img()` 的 URI 分支（那条路每帧都可能重试）。
//!
//! 登记表有预算（[`REGISTRY_BUDGET`]，按字节）：超了把最久没画的先写到数据目录
//! `cache/assets/` 再从内存放掉（`svg()` / `img()` 的名字仍有效，下次画到时
//! `Assets::load` 从磁盘读回来）。公式排版本身另有一份 512 条的缓存（`math.rs`
//! 那侧），这里的预算管的是「解码后的字节」这一份。

use gpui_kit::AssetSource;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// 公式 SVG 的路径前缀（`math/<哈希>.svg`）。
pub const MATH_PREFIX: &str = "math/";
/// 远程图片的路径前缀（`remote/<哈希>.<扩展名>`）。
pub const IMAGE_PREFIX: &str = "remote/";

/// 登记表按字节算的预算；公式 SVG 通常几 KB、远程图片几十 KB～几 MB。
const REGISTRY_BUDGET: usize = 24 * 1024 * 1024;
/// 单份资源大到这个数就不进登记表（也淘汰不进磁盘缓存）：一张图占掉大半个预算，
/// 为了它把别的都挤掉不划算。调用方按「读不出来」处理。
const ENTRY_MAX: usize = 12 * 1024 * 1024;

const KERNEL_LOGOS: &[(&str, &str, &[u8])] = &[
    ("magpie", "kernels/magpie.svg", include_bytes!("../assets/kernels/magpie.svg")),
    ("claude", "kernels/claude.svg", include_bytes!("../assets/kernels/claude.svg")),
    ("opencode", "kernels/opencode.svg", include_bytes!("../assets/kernels/opencode.svg")),
    ("pi", "kernels/pi.svg", include_bytes!("../assets/kernels/pi.svg")),
    ("gemini", "kernels/gemini.svg", include_bytes!("../assets/kernels/gemini.svg")),
];

pub fn kernel_logo_path(id: &str) -> Option<&'static str> {
    KERNEL_LOGOS.iter().find(|(name, _, _)| *name == id).map(|(_, path, _)| *path)
}

/// 内存里登记的资源。字节装在 `Arc` 里，淘汰就是最后一个引用放手。
struct Entry {
    bytes: Arc<Vec<u8>>,
    last_used: Instant,
}

/// 进程级登记表：名字（去掉前缀）→ 内存字节或「已落到磁盘缓存」。
struct Registry {
    map: HashMap<String, Entry>,
    bytes: usize,
    /// 被预算挤出去、写到磁盘缓存的条数（`load` 未命中内存时去那儿找）
    spilled: usize,
    /// 磁盘缓存不可用（建目录失败等）后就不写盘，也不记 spilled
    spill_dir: Option<PathBuf>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        Mutex::new(Registry {
            map: HashMap::new(),
            bytes: 0,
            spilled: 0,
            // 建不成（数据目录不可用）就只留内存表，淘汰即丢、重画时按未登记处理
            spill_dir: claudesktop_core::default_data_dir()
                .ok()
                .map(|d| d.join("cache").join("assets")),
        })
    })
}

impl Registry {
    /// 把最久没用的条目写盘、放掉内存，直到回到预算内。写盘失败的条目留在内存里
    /// （宁可超预算也不能把还会被画的资源直接弄丢），标成刚用过后换下一条试。
    fn trim(&mut self) {
        // 测量钩子：同一可执行文件关回收跑基线，对照开回收的内存差异
        if std::env::var_os("CLAUDESKTOP_DISABLE_RECLAIM").is_some() {
            return;
        }
        while self.bytes > REGISTRY_BUDGET && self.map.len() > 1 {
            let Some((name, oldest)) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(name, e)| (name.clone(), e.last_used))
            else {
                break;
            };
            // 全都一样新（一口气同时插进来的）：不再挤，免得把才登记的那条丢掉
            if Some(oldest) == self.map.values().map(|e| e.last_used).max() {
                break;
            }
            let Some(entry) = self.map.remove(&name) else { break };
            let freed = entry.bytes.len();
            if self.spill(&name, &entry.bytes) {
                self.spilled += 1;
                self.bytes -= freed;
            } else {
                self.map.insert(name, Entry { bytes: entry.bytes, last_used: Instant::now() });
            }
        }
    }

    /// 一份资源写到磁盘缓存。目录建不起来 / 写失败返回 false。
    fn spill(&self, name: &str, bytes: &[u8]) -> bool {
        let Some(dir) = &self.spill_dir else { return false };
        std::fs::create_dir_all(dir).is_ok() && std::fs::write(dir.join(name), bytes).is_ok()
    }

    /// 从磁盘缓存读回（`load` 内存未命中时）。
    fn unspill(&self, name: &str) -> Option<Arc<Vec<u8>>> {
        let dir = self.spill_dir.as_ref()?;
        let bytes = std::fs::read(dir.join(name)).ok()?;
        Some(Arc::new(bytes))
    }
}

/// 登记一张远程图片的原始字节，返回交给 `img()` 的资源名。
///
/// 认不出格式或大到不值得缓存返回 `None`（调用方据此报「读不出来」）。
/// 名字按内容哈希，所以同一张图重复登记是同一个名字、只占一份内存。
pub fn register_image(bytes: &[u8]) -> Option<String> {
    let ext = image_extension(image::guess_format(bytes).ok()?)?;
    // 表里按去掉 `remote/` 前缀的名字存（`load` 也是去掉前缀再查），和公式一样
    let key = format!("{}.{ext}", crate::app::remote_image::hash_bytes(bytes));
    insert(key.clone(), bytes)?;
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

/// 登记一份公式 SVG（已经登记过的同名只刷新使用时刻）。
///
/// 内存只留一份：同一段公式在整段会话里只占一次（排版缓存命中时哈希也一样）。
pub fn register_math(svg: &str) {
    // 表里按去掉 `math/` 前缀的名字存（`load` 也是去掉前缀再查）
    let name = crate::app::math::hash_of(svg);
    insert(name, svg.as_bytes());
}

/// 登记表里加一条（同名只刷新使用时刻）；`None` 的字节大小不入表。
fn insert(name: String, bytes: &[u8]) -> Option<()> {
    if bytes.len() > ENTRY_MAX {
        return None;
    }
    let mut reg = registry().lock().unwrap();
    let now = Instant::now();
    match reg.map.get_mut(&name) {
        Some(entry) => entry.last_used = now,
        None => {
            // 同名字可能已经被写盘淘汰过：从磁盘读回的份没进表，重新插进来
            reg.bytes += bytes.len();
            reg.map.insert(name, Entry { bytes: Arc::new(bytes.to_vec()), last_used: now });
            reg.trim();
        }
    }
    Some(())
}

/// 一份公式 SVG 的资源名。
pub fn math_name(svg: &str) -> String {
    format!("{MATH_PREFIX}{}", crate::app::math::hash_of(svg))
}

/// 资源源：内核标志查静态表，公式和图片查登记表（内存 → 磁盘缓存），其余转发给 gpui-kit。
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, _, bytes)) = KERNEL_LOGOS.iter().find(|(_, name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        for prefix in [MATH_PREFIX, IMAGE_PREFIX] {
            if let Some(rest) = path.strip_prefix(prefix) {
                let mut reg = registry().lock().unwrap();
                if let Some(entry) = reg.map.get_mut(rest) {
                    entry.last_used = Instant::now();
                    let bytes = Arc::clone(&entry.bytes);
                    return Ok(Some(Cow::Owned(bytes.to_vec())));
                }
                // 内存里没有：可能被预算挤到磁盘缓存了
                if let Some(bytes) = reg.unspill(rest) {
                    reg.bytes += bytes.len();
                    reg.map.insert(
                        rest.to_string(),
                        Entry { bytes: Arc::clone(&bytes), last_used: Instant::now() },
                    );
                    reg.trim();
                    return Ok(Some(Cow::Owned(bytes.to_vec())));
                }
                return Ok(None);
            }
        }
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<gpui_kit::SharedString>> {
        if path.trim_end_matches('/') == "kernels" {
            return Ok(KERNEL_LOGOS.iter().map(|(_, name, _)| (*name).into()).collect());
        }
        let math = path.starts_with(MATH_PREFIX) || path.trim_end_matches('/') == "math";
        let image = path.starts_with(IMAGE_PREFIX) || path.trim_end_matches('/') == "remote";
        if math || image {
            let reg = registry().lock().unwrap();
            return Ok(reg
                .map
                .keys()
                .filter(|k| k.ends_with(".svg") == math)
                .map(|k| {
                    let prefix = if math { MATH_PREFIX } else { IMAGE_PREFIX };
                    format!("{prefix}{k}").into()
                })
                .collect());
        }
        gpui_kit::assets::AllAssets.list(path)
    }
}

/// 登记表现状（`CLAUDESKTOP_MEM_REPORT` 打点用）。
pub struct RegistryStats {
    pub entries: usize,
    pub bytes: usize,
    pub budget: usize,
    pub spilled: usize,
}

/// 登记表现状：内存条数 / 字节 / 被挤到磁盘缓存的条数。
pub fn registry_stats() -> RegistryStats {
    let reg = registry().lock().unwrap();
    RegistryStats {
        entries: reg.map.len(),
        bytes: reg.bytes,
        budget: REGISTRY_BUDGET,
        spilled: reg.spilled,
    }
}

/// 一张远程图片已经取回并登记过的字节（`None` = 还没加载 / 加载失败）。
///
/// 「另存为…」写盘要原始字节；画图用 [`crate::app::remote_image::registered_name`]。
pub fn cached(url: &str) -> Option<Vec<u8>> {
    let name = crate::app::remote_image::registered_name(url)?;
    let key = name.strip_prefix(IMAGE_PREFIX)?;
    let mut reg = registry().lock().unwrap();
    if let Some(entry) = reg.map.get_mut(key) {
        entry.last_used = Instant::now();
        return Some(entry.bytes.to_vec());
    }
    reg.unspill(key).map(|arc| arc.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tests 并行跑、登记表是进程级的：所有用例串行持这把锁，磁盘缓存换成本用例
    /// 专属的临时目录（不碰真实数据目录）。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn isolated() -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
        let guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("claudesktop-assets-{:016x}", std::process::id() as u64 ^ nanos()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let mut reg = registry().lock().unwrap();
            reg.map.clear();
            reg.bytes = 0;
            reg.spilled = 0;
            reg.spill_dir = Some(dir.clone());
        }
        (guard, dir)
    }

    fn nanos() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

    #[core::prelude::v1::test]
    fn registered_math_svgs_load_by_name() {
        let (_guard, _dir) = isolated();
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

    #[core::prelude::v1::test]
    fn registered_images_load_by_name() {
        let (_guard, _dir) = isolated();
        // 1×1 的 PNG（gpui 只要能认出格式，登记这一步不解码）
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x2D, 0xB4, 0x00, 0x00,
            0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
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

    #[core::prelude::v1::test]
    fn oversized_entries_are_refused() {
        let (_guard, _dir) = isolated();
        assert!(register_image(&vec![0u8; ENTRY_MAX + 1]).is_none());
    }

    #[core::prelude::v1::test]
    fn budget_evicts_to_disk_and_loads_back() {
        let (_guard, dir) = isolated();
        // 一张能被认出来的最小 PNG：`register_image` 只认格式不解码，头对就行
        let png_1px: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x2D, 0xB4, 0x00, 0x00,
            0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        // 每份 = 假 PNG 头 + 各不相同的填充，保证内容哈希不同；总 40 份 × 1 MB 一定超预算
        let blob = |i: usize| -> Vec<u8> {
            let mut bytes = png_1px.to_vec();
            bytes.extend(std::iter::repeat_n(i as u8, 1024 * 1024));
            bytes
        };
        let mut names = Vec::new();
        for i in 0..40 {
            let bytes = blob(i);
            let Some(name) = register_image(&bytes) else { panic!("第 {i} 份该登记成功") };
            names.push((name, bytes));
        }
        let stats = registry_stats();
        assert!(
            stats.bytes <= REGISTRY_BUDGET + 1024 * 1024,
            "内存应回到预算附近: {}",
            stats.bytes
        );
        assert!(stats.spilled > 0, "应有条目被挤到磁盘");
        assert!(
            dir.join(names[0].0.strip_prefix(IMAGE_PREFIX).unwrap()).exists(),
            "被挤出的应写进磁盘缓存"
        );
        // 被挤出去的条目名字仍有效：load 从磁盘读回，内容一致
        for (name, bytes) in &names {
            assert_eq!(&*Assets.load(name).unwrap().unwrap(), bytes, "{name} 读回应一致");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
