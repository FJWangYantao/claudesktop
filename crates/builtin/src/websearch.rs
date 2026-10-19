//! `web_search` 工具：有 Brave Search API key 时走 Brave（质量好、配额免费档够用），
//! 没有时用 DuckDuckGo HTML 版兜底（不需要 key，可能被限流：结果为空时如实告知）。
//!
//! 结果渲染成带链接的摘要文本给模型（标题 + 网址 + 摘要各一行），
//! 模型要细节再用 `web_fetch` 抓具体页面。

use crate::tools::ToolOutput;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{OnceLock, mpsc};
use std::time::Duration;

/// 最多返回几条结果
pub const SEARCH_MAX_RESULTS: usize = 8;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
/// 响应体上限（DDG 的 HTML 页面有几 MB 的时候）
const SEARCH_MAX_BYTES: u64 = 2 << 20;

/// 环境变量里的 Brave API key（设置页 / 系统环境变量都行）
pub const BRAVE_KEY_ENV: &str = "CLAUDEDESKTOP_BRAVE_KEY";

/// 数据目录（Brave key 文件的位置）；内核启动时设置一次。
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 告诉 websearch 数据目录在哪（读 `brave_key` 文件）。
pub fn set_data_dir(dir: PathBuf) {
    let _ = DATA_DIR.set(dir);
}

/// 内核没起过（刚启动、只用 ACP）时设置页也要能读写 key：退回默认数据目录。
fn data_dir() -> Option<PathBuf> {
    DATA_DIR.get().cloned().or_else(|| claudesktop_core::default_data_dir().ok())
}

/// 取 Brave key：先环境变量，再数据目录的 `brave_key` 文件（设置页写入）。
pub fn brave_key() -> Option<String> {
    if let Ok(k) = std::env::var(BRAVE_KEY_ENV) {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    let file = data_dir()?.join("brave_key");
    std::fs::read_to_string(file).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// 把 key 存进数据目录（None = 删掉，回到 DuckDuckGo 兜底）。
pub fn set_brave_key(key: Option<&str>) -> std::io::Result<()> {
    let Some(dir) = data_dir() else {
        return Err(std::io::Error::other("data directory is not set"));
    };
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("brave_key");
    match key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => std::fs::write(file, k),
        None => match std::fs::remove_file(file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        },
    }
}

/// 一条搜索结果。
struct Hit {
    title: String,
    url: String,
    snippet: String,
}

/// 执行搜索。有 Brave key 走 Brave，否则 DuckDuckGo。
pub fn search(query: &str, should_stop: &dyn Fn() -> bool) -> ToolOutput {
    if query.trim().is_empty() {
        return ToolOutput::fail("web_search: `query` is empty", "参数无效");
    }
    let (tx, rx) = mpsc::channel();
    let owned = query.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(run(&owned, brave_key().as_deref()));
    });
    let hits = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(r) => break r,
            Err(mpsc::RecvTimeoutError::Timeout) if !should_stop() => {}
            _ => return ToolOutput::fail("Cancelled", "已取消"),
        }
    };
    match hits {
        Ok((backend, hits)) => render(query, backend, &hits),
        Err(e) => ToolOutput::fail(format!("web_search failed: {e}"), "搜索失败"),
    }
}

fn run(query: &str, brave_key: Option<&str>) -> Result<(&'static str, Vec<Hit>), String> {
    match brave_key {
        Some(key) => brave(query, key).map(|h| ("brave", h)),
        None => duckduckgo(query).map(|h| ("duckduckgo", h)),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(SEARCH_TIMEOUT))
        .user_agent(concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) ",
            "Chrome/124.0 Safari/537.36"
        ))
        .build()
        .new_agent()
}

/// Brave Search API（`https://api.search.brave.com/res/v1/web/search`）。
fn brave(query: &str, key: &str) -> Result<Vec<Hit>, String> {
    let mut resp = agent()
        .get("https://api.search.brave.com/res/v1/web/search")
        .query("q", query)
        .query("count", SEARCH_MAX_RESULTS.to_string())
        .header("Accept", "application/json")
        .header("X-Subscription-Token", key)
        .call()
        .map_err(|e| format!("brave request: {e}"))?;
    if resp.status().as_u16() != 200 {
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_vec().unwrap_or_default();
        let detail = String::from_utf8_lossy(&body[..body.len().min(300)]);
        // 401/422 大多是 key 不对：提示去设置页改
        return Err(match status {
            401 | 422 => format!(
                "Brave API rejected the key (HTTP {status})；去设置页检查 web_search 的 API key"
            ),
            429 => "Brave API rate limited (HTTP 429)；稍后再试".to_string(),
            _ => format!("Brave API HTTP {status}: {detail}"),
        });
    }
    let body: Value = serde_json::from_slice(
        &resp
            .body_mut()
            .with_config()
            .limit(SEARCH_MAX_BYTES)
            .read_to_vec()
            .map_err(|e| format!("brave response: {e}"))?,
    )
    .map_err(|e| format!("brave response is not JSON: {e}"))?;
    let mut hits = Vec::new();
    if let Some(results) = body.pointer("/web/results").and_then(Value::as_array) {
        for r in results {
            if hits.len() >= SEARCH_MAX_RESULTS {
                break;
            }
            let pick = |path: &str| {
                r.pointer(path).and_then(Value::as_str).map(unescape).unwrap_or_default()
            };
            let url = pick("/url");
            if url.is_empty() {
                continue;
            }
            hits.push(Hit { title: pick("/title"), url, snippet: pick("/description") });
        }
    }
    Ok(hits)
}

/// DuckDuckGo HTML 版（`https://html.duckduckgo.com/html/?q=…`）：免 key 兜底。
fn duckduckgo(query: &str) -> Result<Vec<Hit>, String> {
    let mut resp = agent()
        .post("https://html.duckduckgo.com/html/")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send_form([("q", query)])
        .map_err(|e| format!("duckduckgo request: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .with_config()
        .limit(SEARCH_MAX_BYTES)
        .read_to_vec()
        .map_err(|e| format!("duckduckgo response: {e}"))?;
    if status != 200 {
        return Err(format!(
            "duckduckgo HTTP {status}（无 key 的兜底搜索被限流；可在设置页配置 Brave API key）"
        ));
    }
    Ok(parse_ddg(&String::from_utf8_lossy(&body)))
}

/// 解析 DDG HTML 结果页：每条结果在 `<a class="result__a" href="…">标题</a>`，
/// 摘要在 `<a class="result__snippet">…</a>`；链接是 `/l/?uddg=<编码后的网址>` 要解开。
fn parse_ddg(html: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    // 找出所有结果块（按链接切），每块里再找标题 / 摘要
    let mut rest = html;
    while hits.len() < SEARCH_MAX_RESULTS {
        let Some(start) = rest.find("result__a") else { break };
        rest = &rest[start..];
        let Some(tag_end) = rest.find('>') else { break };
        let attrs = &rest[..tag_end];
        let href = attr(attrs, "href").unwrap_or_default();
        let after = &rest[tag_end + 1..];
        let title_end = after.find("</a>").unwrap_or(after.len());
        let title = crate::fetch::html_to_text(&after[..title_end], "").1.trim().to_string();
        // 摘要：同一条目的 snippet 标签（下一条 result__a 之前找）
        let next = after.find("result__a").unwrap_or(after.len());
        let block = &after[..next];
        let snippet = block
            .find("result__snippet")
            .and_then(|i| {
                let s = &block[i..];
                let open = s.find('>')?;
                let close = s[open..].find("</a>")?;
                Some(
                    crate::fetch::html_to_text(&s[open + 1..open + close], "").1.trim().to_string(),
                )
            })
            .unwrap_or_default();
        let url = ddg_unwrap(&href);
        rest = after;
        if url.is_empty() {
            continue;
        }
        hits.push(Hit { title, url, snippet });
    }
    hits
}

/// DDG 的跳转链接 `/l/?uddg=<urlencode>&rut=…` 解回真网址；广告 / 站内链接（`y.js`）丢弃。
fn ddg_unwrap(href: &str) -> String {
    let trimmed =
        href.trim_start_matches("//duckduckgo.com").trim_start_matches("https://duckduckgo.com");
    // 只解 /l/ 形式的跳转；其余（y.js 广告、# 站内）只在是 http(s) 直链时收下
    if let Some(query) = trimmed.strip_prefix("/l/").and_then(|s| s.split_once('?').map(|(_, q)| q))
    {
        for pair in query.split('&') {
            if let Some(v) = pair.strip_prefix("uddg=") {
                let decoded = percent_decode(v);
                if decoded.starts_with("http") {
                    return decoded;
                }
            }
        }
        return String::new();
    }
    if href.starts_with("http") || href.starts_with("//") && !href.contains("duckduckgo.com") {
        return href.to_string();
    }
    String::new()
}

/// 简易百分号解码（够解 uddg 里的网址）。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// HTML 标签属性值（`href="…"` / `href='…'`）。
fn attr(tag: &str, name: &str) -> Option<String> {
    let mut probe = tag;
    while let Some(i) = probe.find(name) {
        let after = probe[i + name.len()..].trim_start();
        if let Some(v) = after.strip_prefix('=') {
            let v = v.trim_start();
            let quote = v.chars().next()?;
            if quote == '"' || quote == '\'' {
                let end = v[1..].find(quote)? + 1;
                return Some(unescape(&v[1..end]));
            }
        }
        probe = &probe[i + name.len()..];
    }
    None
}

/// 极简实体解码（搜索结果里常见的那几个）。
fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// 结果 → 给模型的文本：`1. 标题\n   网址\n   摘要`，末尾提醒可以 web_fetch。
fn render(query: &str, backend: &str, hits: &[Hit]) -> ToolOutput {
    if hits.is_empty() {
        return ToolOutput::ok(
            format!(
                "No results for \"{query}\" ({backend}). 换个说法再试，或在设置页配置 Brave API key。"
            ),
            "没有结果",
        );
    }
    let mut out = format!("Results for \"{query}\" via {backend}:\n\n");
    for (i, hit) in hits.iter().enumerate() {
        out.push_str(&format!(
            "{}. {}\n",
            i + 1,
            if hit.title.is_empty() { "(untitled)" } else { &hit.title }
        ));
        out.push_str(&format!("   {}\n", hit.url));
        if !hit.snippet.is_empty() {
            out.push_str(&format!("   {}\n", hit.snippet));
        }
        out.push('\n');
    }
    out.push_str("Use web_fetch on a URL above for the full page.");
    let summary = format!("{} 条结果（{}）", hits.len(), backend);
    ToolOutput::ok(out, summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ddg_results() {
        let html = r#"
        <div class="result results_links">
          <h2 class="result__title"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Fserde%2F&amp;rut=abc">serde - <b>Docs.rs</b></a></h2>
          <a class="result__snippet" href="x">Serde &amp; serialization framework</a>
        </div>
        <div class="result">
          <h2><a class="result__a" href="https://example.com/直链">Example &#x27;site&#x27;</a></h2>
          <a class="result__snippet">Second snippet</a>
        </div>
        "#;
        let hits = parse_ddg(html);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "serde - Docs.rs");
        assert_eq!(hits[0].url, "https://docs.rs/serde/");
        assert_eq!(hits[0].snippet, "Serde & serialization framework");
        // 直链不解码、实体解码
        assert_eq!(hits[1].url, "https://example.com/直链");
        assert_eq!(hits[1].title, "Example 'site'");
    }

    #[test]
    fn skips_ads_and_unwraps_redirects() {
        assert_eq!(
            ddg_unwrap("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%20b&rut=x"),
            "https://example.com/a b"
        );
        // 广告 / 站内跳转没有 uddg：丢弃
        assert_eq!(ddg_unwrap("//duckduckgo.com/y.js?ad_domain=x"), "");
        assert_eq!(ddg_unwrap("/?q=x"), "");
        // http 直链原样
        assert_eq!(ddg_unwrap("https://example.com/a"), "https://example.com/a");
    }

    #[test]
    fn renders_hits_for_the_model() {
        let hits = vec![
            Hit {
                title: "Rust".into(),
                url: "https://rust-lang.org".into(),
                snippet: "A language".into(),
            },
            Hit { title: String::new(), url: "https://x.example".into(), snippet: String::new() },
        ];
        let out = render("rust language", "duckduckgo", &hits);
        assert!(out.ok);
        assert!(out.content.contains("1. Rust"));
        assert!(out.content.contains("   https://rust-lang.org"));
        assert!(out.content.contains("2. (untitled)"));
        assert!(out.content.contains("web_fetch"));
        assert_eq!(out.summary, "2 条结果（duckduckgo）");
    }

    #[test]
    fn empty_results_say_so() {
        let out = render("zzz", "brave", &[]);
        assert!(out.ok);
        assert!(out.content.contains("No results"));
    }
}
