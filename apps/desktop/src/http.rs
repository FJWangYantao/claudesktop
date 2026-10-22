//! 给 gpui 的 HTTP 客户端，以及回复里远程图片用的取字节函数
//! （见「设计说明-聊天与Code分离.md」7.5 节）。
//!
//! 两件事：
//!
//! - [`UreqHttpClient`]：`Application::with_http_client` 用。不配的话 gpui 是
//!   `NullHttpClient`，任何 `Resource::Uri` 的图片都取不回来。
//! - [`get_bytes`]：远程图片自己走的那条路。真去请求前要先问用户（占位卡 + 「加载」，
//!   见 `app::remote_image`），所以不能交给 `img("https://…")` 的隐式加载，
//!   而是显式取字节、按内容哈希登记成内嵌资源。
//!
//! 两者都是 `ureq`（和内核同一个 HTTP 栈），代理按 `ureq` 的默认规则读环境变量
//! （`ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY`），这里不另给——给了会盖掉环境变量。

use futures::future::BoxFuture;
use gpui_kit::http_client::http::{HeaderName, HeaderValue, StatusCode};
use gpui_kit::http_client::{AsyncBody, HttpClient, Request, Response};
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

/// 一次 GET 的结果：状态码、响应头、正文（正文最多 [`MAX_BYTES`]）。
struct Fetched {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// 同步发一次 GET（调用方负责放到后台线程）。
fn get(url: &str, accept: &str) -> Result<Fetched, String> {
    let mut resp = agent()
        .get(url)
        .header("Accept", accept)
        .call()
        .map_err(|e| explain(url, &e))?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
        .collect();
    let mut body = Vec::new();
    let too_large = |len: usize| format!("图片太大（超过 {} MB）", MAX_BYTES / 1024 / 1024);
    resp.body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .reader()
        .read_to_end(&mut body)
        .map_err(|e| {
            if body.len() as u64 >= MAX_BYTES {
                too_large(body.len())
            } else {
                format!("读响应失败：{e}")
            }
        })?;
    Ok(Fetched { status, headers, body })
}

/// 取一张远程图片的字节（同步；配 `background_executor().spawn` 用）。
///
/// 非 2xx 也算失败：占位卡上要显示「服务器回了 HTTP 404」这种能看懂的话。
pub fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let fetched = get(url, "image/*")?;
    if !(200..300).contains(&fetched.status) {
        return Err(format!("服务器回了 HTTP {}", fetched.status));
    }
    Ok(fetched.body)
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

/// 走 `ureq` 的 `HttpClient`（gpui 的图片加载器会用它取 `Resource::Uri`）。
pub struct UreqHttpClient;

impl UreqHttpClient {
    /// 包成 `Arc<dyn HttpClient>`，交给 `Application::with_http_client`。
    pub fn shared() -> std::sync::Arc<dyn HttpClient> {
        std::sync::Arc::new(Self)
    }
}

impl HttpClient for UreqHttpClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        static VALUE: OnceLock<HeaderValue> = OnceLock::new();
        Some(VALUE.get_or_init(|| USER_AGENT.parse().expect("静态字符串组成的 user agent")))
    }

    fn proxy(&self) -> Option<&gpui_kit::http_client::Url> {
        // 交给 ureq 读环境变量；返回 Some 会把它盖掉，代理就跟内核别处不一致了
        None
    }

    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        // `Request<AsyncBody>` 不是 Send（body 里可能有非 Send 的 reader）。
        // 图片加载只用 GET、不带 body，所以只把地址和头拷出来。
        let url = req.uri().to_string();
        let accept = req
            .headers()
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("*/*")
            .to_string();
        Box::pin(async move {
            let fetched = smol::unblock(move || get(&url, &accept))
                .await
                .map_err(anyhow::Error::msg)?;
            let mut builder = Response::builder().status(StatusCode::from_u16(fetched.status)?);
            if let Some(map) = builder.headers_mut() {
                for (name, value) in fetched.headers {
                    if let (Ok(name), Ok(value)) = (
                        HeaderName::from_bytes(name.as_bytes()),
                        HeaderValue::from_bytes(value.as_bytes()),
                    ) {
                        map.append(name, value);
                    }
                }
            }
            Ok(builder.body(AsyncBody::from(fetched.body))?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `use gpui_kit::*` 的文件里必须用 `#[core::prelude::v1::test]`（见 AGENTS.md）。
    #[core::prelude::v1::test]
    fn user_agent_carries_the_app_name() {
        assert!(USER_AGENT.starts_with("claudesktop/"), "{USER_AGENT}");
        let client = UreqHttpClient;
        assert_eq!(client.user_agent().unwrap().to_str().unwrap(), USER_AGENT);
    }

    #[core::prelude::v1::test]
    fn client_leaves_proxy_to_ureq() {
        // 返回 Some 会盖掉 ureq 读到的环境变量
        assert!(UreqHttpClient.proxy().is_none());
    }

    #[core::prelude::v1::test]
    fn network_errors_explain_themselves() {
        let text = explain("https://img.example.com/a.png", &ureq::Error::HostNotFound);
        assert!(text.contains("img.example.com"), "{text}");
        assert!(text.contains("检查地址拼写"), "{text}");
    }
}
