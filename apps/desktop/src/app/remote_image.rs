//! 回复里的**远程图片**（见「设计说明-聊天与Code分离.md」7.5 节）。
//!
//! 默认**不自动加载**：模型给的 `https://…` 链接可能是编的，一加载就把用户的 IP 和
//! 「正在看这条回复」告诉了图片所在的服务器。所以 `![](…)` 里的远程地址先画成一张
//! 占位卡「图片 · example.com」+「加载」，点了才请求；设置「外观」里
//! 「自动加载回复里的网络图片」打开后，画到就直接加载（默认关）。
//!
//! 怎么接进来：
//!
//! - 认领在 `MarkdownPlugin::parse` 里（后台线程解析 Markdown，不占 render）：远程地址
//!   从普通图片节点变成自定义节点，**导出的 Markdown 仍是原来的 `![](…)`**。
//! - 真图不交给 `img("https://…")`（它的 URI 分支会在没能量时反复重试、也没法先问用户），
//!   而是自己用 `crate::http` 取字节、按内容哈希登记成内嵌资源（`crate::assets`），
//!   再交给 `img()`。于是「加载」是一个显式的步骤。
//! - 同一条地址只请求一次：结果（登记名或错误原因）记在下面的表里，滚出视口再滚回来直接用。

use crate::theme::{INK_SOFT, LINE, RED, SAND, TEXT_SMALL, rgb};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::{MarkdownNode, MarkdownParseContext, MarkdownPlugin, markdown_ast};
use gpui_kit::component::button::Button;
use gpui_kit::component::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 插件名（也是自定义节点的名字）。
pub const REMOTE_IMAGE: &str = "claudesktop-remote-image";

/// 一张远程图片。
#[derive(Clone, PartialEq)]
pub struct RemoteImage {
    /// 完整地址（加载时原样请求）
    pub url: String,
    /// 占位卡上显示的主机名（`example.com`）
    pub host: String,
    /// 说明文字（`![说明](…)` 里的说明），没有就空
    pub alt: String,
}

impl RemoteImage {
    /// 要不要用户点头：只有 `http(s)://` 算（隐私只在这一类上）。
    /// 本地路径、`data:` 不涉及这件事，交回 gpui 原来的图片行为。
    pub fn qualifies(url: &str) -> bool {
        let lower = url.trim_start().to_ascii_lowercase();
        lower.starts_with("http://") || lower.starts_with("https://")
    }

    /// 从地址里取主机名：`https://a.example.com/x/y.png` -> `a.example.com`。
    pub fn host_of(url: &str) -> String {
        let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        // 去掉 user:pass@ 和端口
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        let host = authority.split(':').next().unwrap_or(authority);
        if host.is_empty() { truncated(url, 60) } else { truncated(host, 60) }
    }

    /// 无障碍名 / 复制出来的文字：说明文字，没有就用「图片 · 域名」。
    fn label(&self) -> String {
        if self.alt.is_empty() { format!("图片 · {}", self.host) } else { self.alt.clone() }
    }
}

fn truncated(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 认领 `![](https://…)`，交给下面画占位卡或真图。
pub struct RemoteImagePlugin;

impl MarkdownPlugin for RemoteImagePlugin {
    fn name(&self) -> &str {
        REMOTE_IMAGE
    }

    fn parse(
        &self,
        node: &markdown_ast::Node,
        _cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let markdown_ast::Node::Image(image) = node else {
            return None;
        };
        if !RemoteImage::qualifies(&image.url) {
            return None;
        }
        let url = image.url.clone();
        let remote = RemoteImage {
            host: RemoteImage::host_of(&url),
            alt: image.alt.clone(),
            url: url.clone(),
        };
        Some(
            MarkdownNode::new(REMOTE_IMAGE, remote.clone())
                .text(remote.label())
                .markdown(format!("![{}]({url})", remote.alt)),
        )
    }

    fn render(&self, node: &MarkdownNode, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(image) = node.data::<RemoteImage>() else {
            return div().child(node.as_text().to_string()).into_any_element();
        };
        // 开了「自动加载」：画到这儿就把请求排上（`start` 自己防重入，这一帧先显示「正在加载」）
        if matches!(state_of(&image.url), State::Idle) && auto_load() {
            start(image.url.clone(), cx);
        }
        match state_of(&image.url) {
            State::Ready(name) => loaded(&name, &image.url),
            state => div().py_1().child(card(image, &state)).into_any_element(),
        }
    }
}

// ---------------------------------------------------------------- 状态与加载

/// 一张远程图片的加载状态（进程内，不落盘：设置里的开关才是长期偏好）。
#[derive(Clone)]
enum State {
    /// 没请求过（占位卡 + 「加载」）
    Idle,
    /// 请求中（占位卡不给按钮）
    Loading,
    /// 解码成功，登记进资源表后的名字（交给 `img()`）
    Ready(String),
    /// 失败原因（给用户看的中文）
    Failed(String),
}

type Table = Mutex<HashMap<String, State>>;

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn state_of(url: &str) -> State {
    table().lock().map_or(State::Idle, |t| t.get(url).cloned().unwrap_or(State::Idle))
}

fn set_state(url: &str, state: State) {
    if let Ok(mut table) = table().lock() {
        table.insert(url.to_string(), state);
    }
}

/// 一张远程图片加载成功后的资源名（`remote/<内容哈希>.<扩展名>`），没加载好是 `None`。
pub(crate) fn registered_name(url: &str) -> Option<String> {
    match state_of(url) {
        State::Ready(name) => Some(name),
        _ => None,
    }
}

/// 「自动加载回复里的网络图片」的进程级开关。
///
/// Markdown 的解析和绘制拿不到 `ChatApp` 的设置，所以设置页切换、启动读设置时
/// 各同步一次（[`set_auto_load`]）。
static AUTO_LOAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn auto_load() -> bool {
    AUTO_LOAD.load(std::sync::atomic::Ordering::Relaxed)
}

/// 同步设置里的「自动加载回复里的网络图片」。
pub fn set_auto_load(on: bool) {
    AUTO_LOAD.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// 排一次请求（结果记进上面的表，成功后让窗口重画一次把图画出来）。
///
/// 字节走 `crate::http::get_bytes`（和 gpui 图片加载器同一套 `ureq` 配置：代理按
/// 环境变量、UA 一致），登记交 `crate::assets::register_image`。
///
/// 状态在全局表里，界面靠一次整体重画把它取走（一张图一次）。
fn start(url: String, cx: &mut App) {
    if matches!(state_of(&url), State::Loading | State::Ready(_)) {
        return;
    }
    set_state(&url, State::Loading);
    cx.spawn(async move |cx| {
        let target = url.clone();
        let bytes =
            cx.background_executor().spawn(async move { crate::http::get_bytes(&target) }).await;
        let state = match bytes.and_then(|bytes| {
            crate::assets::register_image(&bytes)
                .ok_or_else(|| "这张图读不出来（不是常见的图片格式）".to_string())
        }) {
            Ok(name) => State::Ready(name),
            Err(err) => State::Failed(err),
        };
        set_state(&url, state);
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

// ---------------------------------------------------------------- 画

/// 加载好的真图：最宽不超过阅读列，点了看大图（可另存为）。
fn loaded(name: &str, url: &str) -> AnyElement {
    let url = url.to_string();
    let id = hash(url.as_str());
    div()
        .py_1()
        .child(
            img(name.to_string())
                .id(ElementId::Name(format!("remote-img-{id}").into()))
                .max_w(relative(1.))
                .rounded_md()
                .object_fit(ObjectFit::Contain)
                .cursor_pointer()
                .on_click(move |_, window, cx| {
                    super::chat::open_image(super::chat::ImageView::Remote(url.clone()), window, cx)
                }),
        )
        .into_any_element()
}

/// 占位卡：「图片 · 域名」+ 右侧「加载」/「重试」；加载中不给按钮。
fn card(image: &RemoteImage, state: &State) -> AnyElement {
    let (label, tint, action): (String, Rgba, Option<&'static str>) = match state {
        State::Idle => (format!("图片 · {}", image.host), rgb(INK_SOFT), Some("加载")),
        State::Loading => (format!("正在加载 · {}", image.host), rgb(INK_SOFT), None),
        State::Failed(reason) => {
            (format!("图片 · {} — {reason}", image.host), rgb(RED), Some("重试"))
        }
        // 上面已经分流到 `loaded` 了，到这里只是为了穷尽
        State::Ready(_) => (format!("图片 · {}", image.host), rgb(INK_SOFT), Some("加载")),
    };

    let url = image.url.clone();
    let id = hash(&image.url);
    let mut row = div()
        .id(ElementId::Name(format!("remote-card-{id}").into()))
        .h_flex()
        .items_center()
        .gap_2()
        .max_w(relative(0.8))
        .px_2p5()
        .py_1p5()
        .rounded_lg()
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(SAND))
        .text_size(TEXT_SMALL)
        .child(Icon::new(IconAll::Frame).size_3p5().text_color(rgb(INK_SOFT)))
        .child(div().min_w_0().text_color(tint).child(label))
        .child(div().flex_1());
    if let Some(action) = action {
        row = row.child(
            Button::new(SharedString::from(format!("remote-load-{id}")))
                .label(action)
                .small()
                .cursor_pointer()
                .on_click(move |_, _, cx| start(url.clone(), cx)),
        );
    }
    row.into_any_element()
}

/// 地址 → 元素 id 的后半截。
fn hash(url: &str) -> String {
    let hasher = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        url.hash(&mut hasher);
        hasher.finish()
    };
    format!("{hasher:016x}")
}

/// 字节内容 → 资源名里那一截（`assets::register_image` 用：同一张图同名、只占一份）。
pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `use gpui_kit::*` 的文件里必须用 `#[core::prelude::v1::test]`（见 AGENTS.md）。
    #[core::prelude::v1::test]
    fn only_http_counts_as_remote() {
        assert!(RemoteImage::qualifies("https://example.com/a.png"));
        assert!(RemoteImage::qualifies("HTTP://example.com/a.png"));
        // 本地 / 内联不涉及隐私，交回 gpui 原来的图片行为
        assert!(!RemoteImage::qualifies("file:///C:/a.png"));
        assert!(!RemoteImage::qualifies("data:image/png;base64,AAAA"));
        assert!(!RemoteImage::qualifies("./shots/a.png"));
    }

    #[core::prelude::v1::test]
    fn host_is_the_bare_domain() {
        assert_eq!(RemoteImage::host_of("https://example.com/a/b.png"), "example.com");
        assert_eq!(RemoteImage::host_of("https://example.com:8443/a.png"), "example.com");
        assert_eq!(RemoteImage::host_of("https://u:p@cdn.example.com/a.png"), "cdn.example.com");
        assert_eq!(RemoteImage::host_of("https://example.com?x=1"), "example.com");
        // 取不出来也不能是空串（占位卡会只剩「图片 · 」）
        assert!(!RemoteImage::host_of("https:///x").is_empty());
    }

    #[core::prelude::v1::test]
    fn long_hosts_are_truncated() {
        let long = format!("https://{}.com/a.png", "a".repeat(200));
        let host = RemoteImage::host_of(&long);
        assert!(host.chars().count() <= 60, "{host}");
        assert!(host.ends_with('…'));
    }

    #[core::prelude::v1::test]
    fn label_falls_back_to_the_host() {
        let mut image = RemoteImage {
            url: "https://example.com/a.png".into(),
            host: "example.com".into(),
            alt: String::new(),
        };
        assert_eq!(image.label(), "图片 · example.com");
        image.alt = "一张截图".into();
        assert_eq!(image.label(), "一张截图");
    }

    #[core::prelude::v1::test]
    fn states_are_per_url_and_start_one_shy_of_ready() {
        let url = "https://example.com/state-test.png";
        assert!(matches!(state_of(url), State::Idle));
        set_state(url, State::Failed("x".into()));
        assert!(matches!(state_of(url), State::Failed(_)));
        set_state(url, State::Loading);
        assert!(matches!(state_of(url), State::Loading));
        // 别的地址不受影响
        assert!(matches!(state_of("https://example.com/other.png"), State::Idle));
        set_state(url, State::Idle);
    }

    #[core::prelude::v1::test]
    fn auto_load_is_off_by_default() {
        // 默认关：没配过就绝不自动请求（隐私）
        assert!(!auto_load());
        set_auto_load(true);
        assert!(auto_load());
        set_auto_load(false);
    }
}
