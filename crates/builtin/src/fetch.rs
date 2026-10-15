//! `web_fetch` 工具：抓取一个 http(s) 网址，HTML 转成带标题 / 列表 / 链接的纯文本给模型读。
//!
//! 不跑脚本（拿到的是服务器直出的 HTML），只认 UTF-8（其他编码按 UTF-8 宽松解码）。
//! 请求放在后台线程里，停止时不等它结束（全局超时兜底）。

use crate::tools::ToolOutput;
use std::sync::mpsc;
use ureq::ResponseExt as _;
use std::time::Duration;

/// 一次返回给模型的最多字符数；更长的用 `offset` 分页
pub const FETCH_PAGE_CHARS: usize = 20_000;
/// 响应体上限
const FETCH_MAX_BYTES: u64 = 10 << 20;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// 校验并规范网址：只收 http / https，缺协议时补 https。
pub fn normalize_url(raw: &str) -> Result<String, String> {
    let url = raw.trim();
    if url.is_empty() {
        return Err("web_fetch: `url` is empty".into());
    }
    let url = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("web_fetch: only http and https URLs are supported".into());
    }
    if host_of(&url).is_empty() {
        return Err("web_fetch: URL has no host".into());
    }
    Ok(url)
}

/// 网址的主机名（小写，不含端口 / 用户信息）。
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(v6) = host_port.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    host.to_ascii_lowercase()
}

/// 抓取并转换；`offset` 是转换后文本的字符偏移（分页）。
pub fn fetch(url: &str, offset: usize, should_stop: &dyn Fn() -> bool) -> ToolOutput {
    let (tx, rx) = mpsc::channel();
    let owned = url.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(get(&owned));
    });
    let page = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(r) => break r,
            Err(mpsc::RecvTimeoutError::Timeout) if !should_stop() => {}
            _ => return ToolOutput::fail("Cancelled", "已取消"),
        }
    };
    match page {
        Ok(page) => render(url, page, offset),
        Err(e) => ToolOutput::fail(format!("Failed to fetch {url}: {e}"), "抓取失败"),
    }
}

/// 抓到的原始响应。
struct Page {
    status: u16,
    /// 跟随重定向后的最终网址
    final_url: String,
    content_type: String,
    body: Vec<u8>,
}

fn get(url: &str) -> Result<Page, String> {
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(FETCH_TIMEOUT))
        .user_agent(concat!("Mozilla/5.0 (compatible; claudesktop/", env!("CARGO_PKG_VERSION"), ")"))
        .build()
        .new_agent();
    let mut resp = agent
        .get(url)
        .header("Accept", "text/html,application/xhtml+xml,text/markdown;q=0.9,text/plain;q=0.8,*/*;q=0.5")
        .call()
        .map_err(|e| e.to_string())?;
    let final_url = resp.get_uri().to_string();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = resp
        .body_mut()
        .with_config()
        .limit(FETCH_MAX_BYTES)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    Ok(Page {
        status: resp.status().as_u16(),
        final_url,
        content_type,
        body,
    })
}

/// 响应 → 工具结果：HTML 转文本，其他文本原样，二进制拒绝。
fn render(url: &str, page: Page, offset: usize) -> ToolOutput {
    let mime = page.content_type.split(';').next().unwrap_or("").trim().to_string();
    let head = &page.body[..page.body.len().min(8192)];
    let sniff_html = {
        let s = String::from_utf8_lossy(head).trim_start().to_ascii_lowercase();
        s.starts_with("<!doctype html") || s.starts_with("<html")
    };
    let is_html = mime == "text/html" || mime == "application/xhtml+xml" || (mime.is_empty() && sniff_html);
    let is_text = mime.starts_with("text/")
        || mime.ends_with("json")
        || mime.ends_with("xml")
        || mime.ends_with("javascript")
        || (mime.is_empty() && !head.contains(&0));
    if !is_html && !is_text {
        return ToolOutput::fail(
            format!("{url} returned non-text content ({mime}); web_fetch only reads text and HTML"),
            "不是文本",
        );
    }
    let raw = String::from_utf8_lossy(&page.body);
    let (title, text) = if is_html {
        html_to_text(&raw, &page.final_url)
    } else {
        (None, raw.trim().to_string())
    };

    let total = text.chars().count();
    let mut out = format!("URL: {}\n", page.final_url);
    if page.final_url != url {
        out.push_str(&format!("(redirected from {url})\n"));
    }
    if let Some(t) = &title {
        out.push_str(&format!("Title: {t}\n"));
    }
    if !(200..300).contains(&page.status) {
        out.push_str(&format!("HTTP status: {}\n", page.status));
    }
    out.push('\n');
    if offset >= total && total > 0 {
        return ToolOutput::fail(format!("offset {offset} is past the end ({total} chars)"), "超出范围");
    }
    let shown: String = text.chars().skip(offset).take(FETCH_PAGE_CHARS).collect();
    let end = offset + shown.chars().count();
    out.push_str(if shown.is_empty() { "(no text content)" } else { &shown });
    if offset > 0 || end < total {
        out.push_str(&format!(
            "\n\n… showing chars {offset}-{end} of {total}; call web_fetch again with offset={end} to read more"
        ));
    }

    if !(200..300).contains(&page.status) {
        return ToolOutput::fail(out, format!("HTTP {}", page.status));
    }
    ToolOutput::ok(out, format!("{} 字", fmt_count(total)))
}

/// 1234567 → "1,234,567"
fn fmt_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// 内容整段丢掉的标签
const SKIP_TAGS: &[&str] = &[
    "script", "style", "noscript", "svg", "template", "iframe", "canvas", "select", "button",
];
/// 段落级标签：前后空一行
const PARA_TAGS: &[&str] = &[
    "p", "div", "section", "article", "main", "header", "footer", "nav", "aside", "form",
    "table", "ul", "ol", "dl", "blockquote", "figure", "details", "fieldset", "hr",
];
/// 行级标签：前后换行
const LINE_TAGS: &[&str] = &["br", "tr", "dt", "dd", "figcaption", "summary", "caption"];

/// HTML → (标题, 文本)。标题 → `#`，列表 → `- `，链接 → `[文字](绝对网址)`，
/// `<pre>` 保留原样包成代码块，行内 `<code>` 包反引号；脚本 / 样式等整段丢弃。
pub fn html_to_text(html: &str, base: &str) -> (Option<String>, String) {
    let mut w = Writer::default();
    let mut title = None;
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut i = 0;
    let mut pre = 0usize;
    let mut code = 0usize;
    // 打开的 <a>：(链接, `[` 在输出里的位置)
    let mut links: Vec<Option<(String, usize)>> = Vec::new();
    while i < bytes.len() {
        if bytes[i] != b'<' {
            let next = html[i..].find('<').map_or(html.len(), |n| i + n);
            let text = decode_entities(&html[i..next]);
            if pre > 0 {
                w.raw(&text);
            } else {
                w.text(&text);
            }
            i = next;
            continue;
        }
        if lower[i..].starts_with("<!--") {
            i = lower[i..].find("-->").map_or(html.len(), |n| i + n + 3);
            continue;
        }
        // `<` 后面不像标签（`a < b`）或找不到 `>`：当普通文字
        let tag_like = bytes
            .get(i + 1)
            .is_some_and(|&b| b.is_ascii_alphabetic() || matches!(b, b'/' | b'!' | b'?'));
        let Some(close) = find_tag_end(bytes, i + 1).filter(|_| tag_like) else {
            w.text("<");
            i += 1;
            continue;
        };
        let inner = &html[i + 1..close];
        i = close + 1;
        if inner.starts_with('!') || inner.starts_with('?') {
            continue;
        }
        let closing = inner.starts_with('/');
        let body = inner.trim_start_matches('/');
        let name_end = body
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .unwrap_or(body.len());
        let name = body[..name_end].to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }

        if !closing && (SKIP_TAGS.contains(&name.as_str()) || name == "title" || name == "head") {
            // 整段跳到对应的结束标签；<title> 取出来当标题，<head> 里只要标题
            let end_tag = format!("</{name}");
            let end = lower[i..].find(&end_tag).map_or(html.len(), |n| i + n);
            if name == "title" && title.is_none() {
                let t = collapse_ws(&decode_entities(&html[i..end]));
                if !t.is_empty() {
                    title = Some(t);
                }
            } else if name == "head" {
                if let Some(t) = lower[i..end].find("<title").and_then(|n| {
                    let s = i + n;
                    let open_end = find_tag_end(bytes, s + 1)? + 1;
                    let close = lower[open_end..end].find("</title").map(|m| open_end + m)?;
                    Some(collapse_ws(&decode_entities(&html[open_end..close])))
                }) {
                    if title.is_none() && !t.is_empty() {
                        title = Some(t);
                    }
                }
            }
            i = lower[end..].find('>').map_or(html.len(), |n| end + n + 1);
            continue;
        }

        match name.as_str() {
            "pre" => {
                if closing {
                    if pre > 0 {
                        pre -= 1;
                        if pre == 0 {
                            w.end_line();
                            w.raw("```");
                            w.blank_line();
                        }
                    }
                } else {
                    if pre == 0 {
                        w.blank_line();
                        w.raw("```\n");
                    }
                    pre += 1;
                }
            }
            "code" if pre == 0 => {
                if closing {
                    if code > 0 {
                        code -= 1;
                        w.raw("`");
                    }
                } else {
                    code += 1;
                    w.text_glue("`");
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                w.blank_line();
                if !closing {
                    let level = (name.as_bytes()[1] - b'0') as usize;
                    w.raw(&format!("{} ", "#".repeat(level)));
                }
            }
            "li" => {
                if closing {
                    w.end_line();
                } else {
                    w.end_line();
                    w.raw("- ");
                }
            }
            "a" => {
                if closing {
                    if let Some(Some((href, at))) = links.pop() {
                        if w.out[at + 1..].trim().is_empty() {
                            // 没有文字的链接（图标）：去掉 `[`
                            w.out.truncate(at);
                        } else {
                            w.trim_trailing_space();
                            w.raw(&format!("]({href})"));
                        }
                    }
                } else {
                    let href = attr(body, "href")
                        .map(|h| decode_entities(&h))
                        .filter(|h| !h.starts_with('#') && !h.to_ascii_lowercase().starts_with("javascript:"))
                        .map(|h| resolve_url(base, h.trim()));
                    links.push(href.map(|h| {
                        w.text_glue("[");
                        (h, w.out.len() - 1)
                    }));
                }
            }
            "img" if !closing => {
                if let Some(alt) = attr(body, "alt").map(|a| collapse_ws(&decode_entities(&a))) {
                    if !alt.is_empty() {
                        w.text(&format!("[image: {alt}]"));
                    }
                }
            }
            "td" | "th" if !closing => w.text(" | "),
            n if PARA_TAGS.contains(&n) => w.blank_line(),
            n if LINE_TAGS.contains(&n) => w.end_line(),
            _ => {}
        }
    }
    (title, w.finish())
}

/// 从 `start` 起找标签的 `>`（跳过引号里的 `>`）。
fn find_tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote = None;
    for (k, &b) in bytes.iter().enumerate().skip(start) {
        match (quote, b) {
            (None, b'"' | b'\'') => quote = Some(b),
            (Some(q), _) if b == q => quote = None,
            (None, b'>') => return Some(k),
            _ => {}
        }
    }
    None
}

/// 取属性值（`name="v"` / `name='v'` / `name=v`）。
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(n) = lower[from..].find(name) {
        let at = from + n;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let v_start = tag.len() - rest.len() + 1;
        let v = tag[v_start..].trim_start();
        return Some(match v.chars().next() {
            Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or("").to_string(),
            _ => v.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or("").to_string(),
        });
    }
    None
}

/// 相对链接 → 绝对网址。
fn resolve_url(base: &str, href: &str) -> String {
    if href.contains("://") || href.starts_with("mailto:") || href.starts_with("data:") {
        return href.to_string();
    }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    let origin_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..origin_end]);
    if let Some(r) = href.strip_prefix("//") {
        return format!("{scheme}://{r}");
    }
    if href.starts_with('/') {
        return format!("{origin}{href}");
    }
    let path = rest[origin_end..].split(['?', '#']).next().unwrap_or("");
    if href.starts_with('?') {
        return format!("{origin}{path}{href}");
    }
    let dir = &path[..path.rfind('/').map_or(0, |n| n + 1)];
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let (href_path, suffix) = match href.find(['?', '#']) {
        Some(n) => (&href[..n], &href[n..]),
        None => (href, ""),
    };
    for seg in href_path.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let trailing = if href_path.ends_with('/') && !parts.is_empty() { "/" } else { "" };
    format!("{origin}/{}{trailing}{suffix}", parts.join("/"))
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// HTML 实体解码（数字实体 + 常见命名实体；认不出的原样保留）。
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(n) = rest.find('&') {
        out.push_str(&rest[..n]);
        rest = &rest[n..];
        let end = rest[1..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '#')
            .map_or(rest.len(), |e| e + 1);
        let decoded = Some(end).filter(|&e| e > 1 && e <= 32).and_then(|e| {
            let name = &rest[1..e];
            let c = if let Some(num) = name.strip_prefix('#') {
                let v = match num.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num.parse().ok(),
                };
                v.and_then(char::from_u32)
            } else {
                Some(match name {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    "nbsp" => ' ',
                    "mdash" => '—',
                    "ndash" => '–',
                    "hellip" => '…',
                    "copy" => '©',
                    "reg" => '®',
                    "trade" => '™',
                    "middot" => '·',
                    "laquo" => '«',
                    "raquo" => '»',
                    "lsquo" => '‘',
                    "rsquo" => '’',
                    "ldquo" => '“',
                    "rdquo" => '”',
                    "times" => '×',
                    _ => return None,
                })
            }?;
            // 分号可省
            let consumed = if rest[e..].starts_with(';') { e + 1 } else { e };
            Some((c, consumed))
        });
        match decoded {
            Some((c, consumed)) => {
                out.push(c);
                rest = &rest[consumed..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 拼输出：普通文字折叠空白，换行 / 空行去重。
#[derive(Default)]
struct Writer {
    out: String,
}

impl Writer {
    fn at_line_start(&self) -> bool {
        self.out.is_empty() || self.out.ends_with('\n')
    }

    /// 普通文字：空白折成一个空格，行首和链接的 `[` 后不留空格
    fn text(&mut self, s: &str) {
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_whitespace() {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                if !self.at_line_start() && !self.out.ends_with([' ', '[']) {
                    self.out.push(' ');
                }
            } else {
                self.out.push(c);
            }
        }
    }

    /// 贴着后文的记号（`[`、开头的反引号）：保留它前面应有的空格
    fn text_glue(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn raw(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn trim_trailing_space(&mut self) {
        while self.out.ends_with(' ') {
            self.out.pop();
        }
    }

    fn end_line(&mut self) {
        self.trim_trailing_space();
        if !self.at_line_start() {
            self.out.push('\n');
        }
    }

    fn blank_line(&mut self) {
        self.end_line();
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn finish(self) -> String {
        // 每行去掉首尾空格（代码块里的缩进保留），连续空行压成一个
        let mut out = String::new();
        let mut in_code = false;
        let mut blank = 0;
        for line in self.out.lines() {
            if line.starts_with("```") {
                in_code = !in_code;
            }
            let line = if in_code { line.trim_end() } else { line.trim() };
            // 空列表项（`<li>` 里只有图标）整行去掉
            if line == "-" && !in_code {
                continue;
            }
            if line.is_empty() && !in_code {
                blank += 1;
                continue;
            }
            if !out.is_empty() && blank > 0 {
                out.push('\n');
            }
            blank = 0;
            out.push_str(line);
            out.push('\n');
        }
        out.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_checked_and_hosts_extracted() {
        assert_eq!(normalize_url("example.com/a").unwrap(), "https://example.com/a");
        assert!(normalize_url("ftp://x.org").is_err());
        assert!(normalize_url("file:///etc/passwd").is_err());
        assert!(normalize_url("  ").is_err());
        assert_eq!(host_of("https://User@Docs.RS:443/x?y"), "docs.rs");
        assert_eq!(host_of("http://[::1]:8080/"), "::1");
        assert_eq!(resolve_url("https://a.com/x/y/z.html?q", "../b.html#h"), "https://a.com/x/b.html#h");
        assert_eq!(resolve_url("https://a.com/x/", "c/"), "https://a.com/x/c/");
        assert_eq!(resolve_url("https://a.com/x/y", "/root"), "https://a.com/root");
        assert_eq!(resolve_url("https://a.com/x/y", "//cdn.com/z"), "https://cdn.com/z");
        assert_eq!(resolve_url("https://a.com", "p"), "https://a.com/p");
    }

    #[test]
    fn html_becomes_readable_text() {
        let html = r#"<!DOCTYPE html><html><head><title> Rust &amp; Co </title>
            <style>body{color:red}</style><script>var x = "<p>no</p>";</script></head>
            <body><nav><a href="/"><img src="logo.png"></a></nav>
            <h1>Hello&nbsp;world</h1>
            <p>Read   the <a href="docs/intro.html">intro
              guide</a> and use <code>cargo build</code>.<!-- hidden --></p>
            <ul><li>one</li><li><img src="x.png"></li><li>two &#x4e2d;&#25991;</li></ul>
            <pre><code>fn main() {
    println!("&lt;hi&gt;");
}</code></pre>
            <table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>
            <p>a &lt; b &unknown; c</p></body></html>"#;
        let (title, text) = html_to_text(html, "https://example.com/book/index.html");
        assert_eq!(title.as_deref(), Some("Rust & Co"));
        assert_eq!(
            text,
            "# Hello world\n\n\
             Read the [intro guide](https://example.com/book/docs/intro.html) and use `cargo build`.\n\n\
             - one\n- two 中文\n\n\
             ```\nfn main() {\n    println!(\"<hi>\");\n}\n```\n\n\
             | A | B\n| 1 | 2\n\n\
             a < b &unknown; c"
        );
    }

    #[test]
    fn non_html_and_paging() {
        let page = |ct: &str, body: &str| Page {
            status: 200,
            final_url: "https://x.org/a".into(),
            content_type: ct.into(),
            body: body.as_bytes().to_vec(),
        };
        let out = render("https://x.org/a", page("application/json", "{\"a\":1}"), 0);
        assert!(out.ok && out.content.ends_with("{\"a\":1}"), "{}", out.content);
        let out = render("https://x.org/a", page("image/png", "\u{0}PNG"), 0);
        assert!(!out.ok);

        let long = "x".repeat(FETCH_PAGE_CHARS + 10);
        let out = render("https://x.org/a", page("text/plain", &long), 0);
        assert!(out.content.contains(&format!("offset={FETCH_PAGE_CHARS}")));
        assert_eq!(out.summary, format!("{} 字", fmt_count(FETCH_PAGE_CHARS + 10)));
        let out = render("https://x.org/a", page("text/plain", &long), FETCH_PAGE_CHARS);
        assert!(out.content.contains(&format!("showing chars {FETCH_PAGE_CHARS}-{}", FETCH_PAGE_CHARS + 10)));

        let mut missing = page("text/html", "<p>gone</p>");
        missing.status = 404;
        let out = render("https://x.org/a", missing, 0);
        assert!(!out.ok && out.summary == "HTTP 404" && out.content.contains("gone"));
    }

    /// 联网抓真实网页看转换效果：
    /// `CLAUDESKTOP_FETCH_URL=https://… cargo test -p claudesktop-builtin live_fetch -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_fetch() {
        let url = std::env::var("CLAUDESKTOP_FETCH_URL").unwrap_or_else(|_| "https://www.rust-lang.org/".into());
        let out = fetch(&normalize_url(&url).unwrap(), 0, &|| false);
        println!("[{}] {}\n{}", out.ok, out.summary, out.content);
        assert!(out.ok);
    }
}
