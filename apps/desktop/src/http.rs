//! 回复里远程图片用的取字节函数（见「设计说明-聊天与Code分离.md」7.5 节）。
//!
//! 真去请求前要先问用户（占位卡 + 「加载」，见 `app::remote_image`），所以不能交给
//! `img("https://…")` 的隐式加载，而是显式取字节、按内容哈希登记成内嵌资源。
//! 也因此**不给 gpui 配 `HttpClient`**：保持默认的 `NullHttpClient`，回复里漏网的远程图
//! （比如原始 HTML 的 `<img src="https://…">`，插件认领不到）就取不回来，不会绕过「先问用户」。
//! 新的网络图片来源一律走 [`get_bytes`]。
//!
//! 走 `ureq`（和内核同一个 HTTP 栈），代理按 `ureq` 的默认规则读环境变量
//! （`ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY`），这里不另给——给了会盖掉环境变量。

use std::io::Read as _;
use std::sync::OnceLock;
use std::time::Duration;

/// 取一张图最多等多久：图片可能几 MB，比接口请求宽一点，但也不能无限等。
const TIMEOUT: Duration = Duration::from_secs(30);
/// 单张图的字节上限（4 MiB）：超过就当加载失败，别把内存和帧时间拖死。
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// 跟几次重定向（图片站很常见：原地址 302 到 CDN）。
const MAX_REDIRECTS: u32 = 10;

/// 请求头里的应用身份（图片服务器在日志里看到的是谁）。
pub const USER_AGENT: &str = concat!("claudesktop/", env!("CARGO_PKG_VERSION"));

/// 共用一条连接池：同一个图床连取几张图时少握几次手。
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            // 4xx / 5xx 自己看状态码，不当成错误（要把状态原样报出去）
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(TIMEOUT))
            .user_agent(USER_AGENT)
            .max_redirects(MAX_REDIRECTS)
            .build()
            .new_agent()
    })
}

/// 取一张远程图片的字节（同步；配 `background_executor().spawn` 用）。
///
/// 非 2xx 也算失败：占位卡上要显示「服务器回了 HTTP 404」这种能看懂的话。
pub fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let mut resp =
        agent().get(url).header("Accept", "image/*").call().map_err(|e| explain(url, &e))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("服务器回了 HTTP {status}"));
    }
    let mut body = Vec::new();
    resp.body_mut().with_config().limit(MAX_BYTES).reader().read_to_end(&mut body).map_err(
        |e| {
            // 超限时 ureq 报 `BodyExceedsLimit`（包在 io::Error 里），已读的量不一定到顶
            let over = e
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<ureq::Error>())
                .is_some_and(|inner| matches!(inner, ureq::Error::BodyExceedsLimit(_)));
            if over || body.len() as u64 >= MAX_BYTES {
                format!("图片太大（超过 {} MB）", MAX_BYTES / 1024 / 1024)
            } else {
                format!("读响应失败：{e}")
            }
        },
    )?;
    Ok(body)
}

/// 常见网络错误翻成能照着做的话（和 `mcp_http::send_error` 一个路子）。
fn explain(url: &str, e: &ureq::Error) -> String {
    match e {
        ureq::Error::HostNotFound => {
            let host = gpui_kit::http_client::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| url.to_string());
            format!("找不到 {host}：检查地址拼写和网络")
        }
        ureq::Error::ConnectionFailed => "连不上这个地址：网络不通，或者图床拦了".into(),
        ureq::Error::Timeout(_) => format!("等超过 {} 秒还没下完", TIMEOUT.as_secs()),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_carries_the_app_name() {
        assert!(USER_AGENT.starts_with("claudesktop/"), "{USER_AGENT}");
    }

    #[test]
    fn network_errors_explain_themselves() {
        let text = explain("https://img.example.com/a.png", &ureq::Error::HostNotFound);
        assert!(text.contains("img.example.com"), "{text}");
        assert!(text.contains("检查地址拼写"), "{text}");
    }
}
