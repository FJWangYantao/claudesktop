//! `web_search` 工具：三个后端按用户排的顺序依次尝试，前一个失败（或没配好）就降级到下一个，
//! 结果里如实写明实际用了哪个、谁被跳过、为什么（见 [`search`] 的说明）。
//!
//! 1. **DeepSeek 官方搜索**（`POST /v1/tools/search`）：复用 DeepSeek 的 `sk-` key，中文结果好，
//!    按次计费（1000 次 $0.75，见「设计说明-DeepSeek官方搜索.md」）；
//! 2. **Brave Search API**：质量好，但要用户自己申请 key；
//! 3. **DuckDuckGo HTML 版**：不要 key 的最后兜底，可能被限流，结果是抓 HTML 来的。
//!
//! 默认顺序是 Brave → DuckDuckGo（和加后端之前的行为一致，升级不会悄悄花钱），
//! 用户在设置页选了 DeepSeek（「先用哪个」或「也允许」）才走它，有没有 key 不算数。选择存在数据目录的 `search.json`。
//!
//! 结果渲染成带链接的摘要文本给模型（标题 + 网址 + 摘要各一行），
//! 模型要细节再用 `web_fetch` 抓具体页面。

use crate::tools::ToolOutput;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock, mpsc};
use std::time::Duration;

/// 最多返回几条结果
pub const SEARCH_MAX_RESULTS: usize = 8;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
/// 响应体上限（DDG 的 HTML 页面有几 MB 的时候）
const SEARCH_MAX_BYTES: u64 = 2 << 20;
/// 一条摘要最多给模型多少字（DeepSeek 的 `content` 能好几千字，全塞进上下文不划算）
const SNIPPET_CHARS: usize = 300;

/// DeepSeek 官方搜索端点（Fetch 是同目录的 `/v1/tools/fetch`，本版没接）
const DEEPSEEK_SEARCH_URL: &str = "https://api.deepseek.com/v1/tools/search";

/// 环境变量里的 Brave API key（设置页 / 系统环境变量都行）
pub const BRAVE_KEY_ENV: &str = "CLAUDEDESKTOP_BRAVE_KEY";

/// 环境变量里的 DeepSeek 搜索 key（没填的话用 provider 配置里那把）
pub const DEEPSEEK_KEY_ENV: &str = "CLAUDEDESKTOP_DEEPSEEK_KEY";

/// 数据目录（key 文件和 `search.json` 的位置）；内核启动时设置一次。
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 告诉 websearch 数据目录在哪（读 `brave_key` / `deepseek_key` 文件、`search.json`）。
pub fn set_data_dir(dir: PathBuf) {
    let _ = DATA_DIR.set(dir);
}

/// 内核没起过（刚启动、只用 ACP）时设置页也要能读写：退回默认数据目录。
pub fn data_dir() -> Option<PathBuf> {
    DATA_DIR.get().cloned().or_else(|| claudesktop_core::default_data_dir().ok())
}

/// [`set_data_dir`] 之后才能定位文件；设置页先问这个，免得写进别的地方。
pub fn data_dir_known() -> bool {
    DATA_DIR.get().is_some()
}

/// 取 Brave key：先环境变量，再数据目录的 `brave_key` 文件（设置页写入）。
pub fn brave_key() -> Option<String> {
    env_key(BRAVE_KEY_ENV).or_else(|| key_file("brave_key"))
}

fn env_key(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

fn key_file(name: &str) -> Option<String> {
    let file = data_dir()?.join(name);
    std::fs::read_to_string(file).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// 把 Brave key 存进数据目录（None = 删掉，退回没有 Brave 的搜索顺序）。
pub fn set_brave_key(key: Option<&str>) -> std::io::Result<()> {
    set_key_file("brave_key", key)
}

fn set_key_file(name: &str, key: Option<&str>) -> std::io::Result<()> {
    let Some(dir) = data_dir() else {
        return Err(std::io::Error::other("data directory is not set"));
    };
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(name);
    match key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => std::fs::write(file, k),
        None => match std::fs::remove_file(file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        },
    }
}

/// 搜索后端。**声明顺序就是固定的降级顺序**`DeepSeek → Brave → DuckDuckGo`：
/// 用户选的是「允许哪些」和「优先用哪个」，降级链仍按这个顺序排。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SearchBackend {
    /// DeepSeek 官方搜索（复用 DeepSeek 的 `sk-` key，按次计费）
    #[serde(rename = "deepseek")]
    DeepSeek,
    /// Brave Search API（要自己的 key）
    #[serde(rename = "brave")]
    Brave,
    /// DuckDuckGo HTML（不要 key，可能被限流）
    #[serde(rename = "duckduckgo")]
    DuckDuckGo,
}

impl SearchBackend {
    /// 固定的降级顺序（也是设置页里列表的顺序）。
    pub const ALL: [SearchBackend; 3] =
        [SearchBackend::DeepSeek, SearchBackend::Brave, SearchBackend::DuckDuckGo];

    /// 结果和日志里用的名字（和以前的 `"brave"` / `"duckduckgo"` 一致）。
    pub fn id(self) -> &'static str {
        match self {
            SearchBackend::DeepSeek => "deepseek",
            SearchBackend::Brave => "brave",
            SearchBackend::DuckDuckGo => "duckduckgo",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SearchBackend::DeepSeek => "DeepSeek",
            SearchBackend::Brave => "Brave",
            SearchBackend::DuckDuckGo => "DuckDuckGo",
        }
    }

    /// 不要用户自己申请 key 的那个兜底后端。
    pub fn needs_key(self) -> bool {
        self != SearchBackend::DuckDuckGo
    }
}

/// 搜索后端的选择，存在数据目录的 `search.json`。**只存选择，不存 key**
/// （key 在 `brave_key` / `deepseek_key` 文件里，或跟着 provider 配置走）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// 优先用哪个后端（要它也被允许才作数）
    pub prefer: SearchBackend,
    /// 允许哪些后端：不允许的直接跳过，也不在降级链里。
    /// DeepSeek 按次计费，**只看用户选没选**（这个开关或 `prefer`），配了 key 不算允许
    pub allow_deepseek: bool,
    pub allow_brave: bool,
    pub allow_duckduckgo: bool,
}

impl Default for SearchConfig {
    /// 默认和加 DeepSeek 之前一致：有 Brave key 走 Brave，没有走 DuckDuckGo。
    /// DeepSeek **默认不选**——它是按次计费的新花费，得用户自己去勾。
    fn default() -> Self {
        Self {
            prefer: SearchBackend::Brave,
            allow_deepseek: false,
            allow_brave: true,
            allow_duckduckgo: true,
        }
    }
}

impl SearchConfig {
    /// 实际尝试顺序：允许的按固定顺序排，`prefer` 若被允许就提到最前。
    pub fn order(&self) -> Vec<SearchBackend> {
        let mut list: Vec<SearchBackend> = SearchBackend::ALL
            .into_iter()
            .filter(|b| match b {
                SearchBackend::Brave => self.allow_brave,
                SearchBackend::DuckDuckGo => self.allow_duckduckgo,
                SearchBackend::DeepSeek => self.deepseek_allowed(),
            })
            .collect();
        if self.deepseek_allowed() {
            list.retain(|b| *b != SearchBackend::DeepSeek);
            let at = if self.prefer == SearchBackend::DeepSeek { 0 } else { list.len() };
            list.insert(at, SearchBackend::DeepSeek);
        } else if let Some(at) = list.iter().position(|b| *b == self.prefer) {
            let picked = list.remove(at);
            list.insert(0, picked);
        }
        list
    }

    /// DeepSeek 算不算「被允许」：开关开着，或优先项选了它（都是用户明确要的）。
    fn deepseek_allowed(&self) -> bool {
        self.allow_deepseek || self.prefer == SearchBackend::DeepSeek
    }
}

fn search_json() -> Option<PathBuf> {
    Some(data_dir()?.join("search.json"))
}

/// 读选择：文件没有 / 读坏了就用默认值（不报错——搜索是随时在用的）。
pub fn load() -> SearchConfig {
    search_json()
        .and_then(|p| std::fs::read_to_string(p).ok())
        // 用户手写的 JSON：写坏了当没配，退回默认顺序
        .and_then(|text| serde_json::from_str::<SearchConfig>(&text).ok())
        .unwrap_or_default()
}

/// 存选择（只有 `prefer` / `allow_*`，key 不落这个文件）。
pub fn save(cfg: &SearchConfig) -> std::io::Result<()> {
    let Some(dir) = data_dir() else {
        return Err(std::io::Error::other("data directory is not set"));
    };
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(cfg).unwrap_or_default();
    std::fs::write(dir.join("search.json"), text + "\n")
}

/// DeepSeek 搜索单独的 key（数据目录的 `deepseek_key` 文件 / 环境变量）。
pub fn own_deepseek_key() -> Option<String> {
    env_key(DEEPSEEK_KEY_ENV).or_else(|| key_file("deepseek_key"))
}

/// 把 DeepSeek 搜索的 key 存进数据目录（None = 删掉，改用 provider 配置里那把）。
pub fn set_own_deepseek_key(key: Option<&str>) -> std::io::Result<()> {
    set_key_file("deepseek_key", key)
}

/// 内核交过来的「provider 配置里那把 DeepSeek key」（零配置用，不落盘）。
static KEY_HINT: RwLock<Option<String>> = RwLock::new(None);

/// 内核启动时告诉 websearch：provider 档案里 DeepSeek 端点的 key 是哪把（None = 没配）。
pub fn set_key_hint(key: Option<String>) {
    let key = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    if let Ok(mut slot) = KEY_HINT.write() {
        *slot = key;
    }
}

/// DeepSeek 搜索实际用的 key：单独填的优先，没有就用 provider 档案那把。
pub fn deepseek_key() -> Option<String> {
    own_deepseek_key().or_else(|| KEY_HINT.read().ok().and_then(|k| k.clone()))
}

/// 一条搜索结果。
struct Hit {
    title: String,
    url: String,
    snippet: String,
}

/// 执行搜索：按配置的顺序逐个试，第一个成功的就用，结果末尾写明用了哪个、
/// 前面谁被跳过 / 失败了。全都试过仍失败才返回错误（错误里带每个后端为什么不行）。
pub fn search(query: &str, should_stop: &dyn Fn() -> bool) -> ToolOutput {
    if query.trim().is_empty() {
        return ToolOutput::fail("web_search: `query` is empty", "参数无效");
    }
    let cfg = load();
    let order = cfg.order();
    if order.is_empty() {
        return ToolOutput::fail(
            "web_search: no backend is enabled. Ask the user to turn on at least one search backend \
             in Settings › Web search.",
            "没有可用的搜索后端",
        );
    }
    let (tx, rx) = mpsc::channel();
    let owned = query.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(run(&owned, &order));
    });
    let outcome = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(r) => break r,
            Err(mpsc::RecvTimeoutError::Timeout) if !should_stop() => {}
            _ => return ToolOutput::fail("Cancelled", "已取消"),
        }
    };
    match outcome {
        Ok((backend, hits, notes)) => render(query, backend, &hits, &notes),
        Err(e) => ToolOutput::fail(format!("web_search failed: {e}"), "搜索失败"),
    }
}

/// 跑一遍降级链。返回（用了哪个后端，结果，过程中的说明）。
fn run(
    query: &str,
    order: &[SearchBackend],
) -> Result<(SearchBackend, Vec<Hit>, Vec<String>), String> {
    let mut notes = Vec::new();
    for backend in order {
        let attempt = match backend {
            SearchBackend::DeepSeek => match deepseek_key() {
                // 没配 key：跳过，不算失败（设置页里有灰字说明）
                None => {
                    notes.push("DeepSeek skipped: no API key".to_string());
                    continue;
                }
                Some(key) => deepseek(query, &key),
            },
            SearchBackend::Brave => match brave_key() {
                None => {
                    notes.push("Brave skipped: no API key".to_string());
                    continue;
                }
                Some(key) => brave(query, &key),
            },
            SearchBackend::DuckDuckGo => duckduckgo(query),
        };
        match attempt {
            Ok(hits) => return Ok((*backend, hits, notes)),
            Err(e) => notes.push(format!("{} failed: {e}", backend.label())),
        }
    }
    Err(format!("all backends failed ({})", notes.join("; ")))
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

/// DeepSeek 官方搜索（`POST https://api.deepseek.com/v1/tools/search`）。
///
/// 请求只要 `query` + `count`（要结果列表，不要它服务端的回答）；返回 `results[]`，
/// 每项 `title` / `url` / `snippet`（短，100~200 字）/ `summary`（长，600~800 字，给模型看的）。
/// 摘要优先用 `summary`（信息更全），截到 [`SNIPPET_CHARS`]。
fn deepseek(query: &str, key: &str) -> Result<Vec<Hit>, String> {
    let mut resp = agent()
        .post(DEEPSEEK_SEARCH_URL)
        .header("Authorization", &format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(
            serde_json::json!({
                "query": query,
                "count": SEARCH_MAX_RESULTS,
            })
            .to_string(),
        )
        .map_err(|e| {
            let reason = explain_search_error("deepseek", None, &e.to_string());
            format!("deepseek request: {reason}")
        })?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .with_config()
        .limit(SEARCH_MAX_BYTES)
        .read_to_vec()
        .map_err(|e| format!("deepseek response: {e}"))?;
    if status != 200 {
        let text = String::from_utf8_lossy(&body);
        return Err(explain_search_error("deepseek", Some(status), &text));
    }
    let body: Value =
        serde_json::from_slice(&body).map_err(|e| format!("deepseek response is not JSON: {e}"))?;
    Ok(parse_deepseek(&body))
}

/// 解析 DeepSeek 的 `results[]`（字段缺省、超量都按正常情况处理）。
fn parse_deepseek(body: &Value) -> Vec<Hit> {
    let mut hits = Vec::new();
    let Some(results) = body.get("results").and_then(Value::as_array) else {
        return hits;
    };
    for r in results {
        if hits.len() >= SEARCH_MAX_RESULTS {
            break;
        }
        let pick =
            |name: &str| r.get(name).and_then(Value::as_str).map(str::trim).unwrap_or_default();
        let url = pick("url");
        if url.is_empty() {
            continue;
        }
        // 长摘要在前、短横线在后；都空就空着（不编）
        let text = match (pick("summary"), pick("snippet")) {
            ("", s) => s.to_string(),
            (m, "") => m.to_string(),
            (m, s) => format!("{m}（{s}）"),
        };
        hits.push(Hit {
            title: pick("title").to_string(),
            url: url.to_string(),
            snippet: clip(&text),
        });
    }
    hits
}

/// 把状态码 / 传输错误译成能照着做的话（给模型和用户看，中英混排和现有提示一致）。
fn explain_search_error(backend: &str, status: Option<u16>, detail: &str) -> String {
    let label = match backend {
        "deepseek" => "DeepSeek",
        "brave" => "Brave",
        _ => "DuckDuckGo",
    };
    let snippet: String = detail.trim().chars().take(200).collect();
    match status {
        Some(400) => {
            format!("{label} rejected the request (HTTP 400)：查询写法不合它的要求（{snippet}）")
        }
        // DuckDuckGo 没有 key，它的 401 按普通状态码报
        Some(401) if backend != "duckduckgo" => {
            format!(
                "{label} rejected the API key (HTTP 401)；去设置页「网页搜索」检查 {label} 的 key"
            )
        }
        Some(402) => format!("{label} account has no balance (HTTP 402)；去平台充值后重试"),
        Some(403) => format!("{label} refused the request (HTTP 403)：{snippet}"),
        Some(429) => format!("{label} rate limited (HTTP 429)；稍等一会儿再搜，或换别的后端"),
        Some(code) if code >= 500 => {
            format!("{label} is temporarily unavailable (HTTP {code})；稍后再试")
        }
        Some(code) => format!("{label} HTTP {code}: {snippet}"),
        None => format!("{label} request failed: {snippet}"),
    }
}

/// 摘要截到 [`SNIPPET_CHARS`] 个字符（按字符不按字节，中文不会被截半个）。
fn clip(text: &str) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= SNIPPET_CHARS {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
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
        .map_err(|e| {
            let reason = explain_search_error("brave", None, &e.to_string());
            format!("brave request: {reason}")
        })?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .with_config()
        .limit(SEARCH_MAX_BYTES)
        .read_to_vec()
        .map_err(|e| format!("brave response: {e}"))?;
    if status != 200 {
        let detail = String::from_utf8_lossy(&body);
        return Err(explain_search_error("brave", Some(status), &detail));
    }
    let body: Value =
        serde_json::from_slice(&body).map_err(|e| format!("brave response is not JSON: {e}"))?;
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
        // 无 key 的兜底被限流是常事：提示里直接说怎么办
        return Err(format!(
            "{}（无 key 的兜底搜索被限流；可在设置页换 Brave 或 DeepSeek）",
            explain_search_error("duckduckgo", Some(status), "")
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

/// 结果 → 给模型的文本：`1. 标题\n   网址\n   摘要`，末尾提醒可以 web_fetch；
/// 降级说明（谁被跳过、谁失败了）单独一行写在最前面——别让用户以为一直是 DeepSeek 在搜。
fn render(query: &str, backend: SearchBackend, hits: &[Hit], notes: &[String]) -> ToolOutput {
    let mut out = String::new();
    for note in notes {
        out.push_str(&format!("({note})\n"));
    }
    if !notes.is_empty() {
        out.push('\n');
    }
    if hits.is_empty() {
        out.push_str(&format!(
            "No results for \"{query}\" ({}). 换个说法再试，或在设置页换个搜索后端。",
            backend.id()
        ));
        let summary = if notes.is_empty() {
            "没有结果".to_string()
        } else {
            "没有结果（降级过）".to_string()
        };
        return ToolOutput::ok(out, summary);
    }
    out.push_str(&format!("Results for \"{query}\" via {}:\n\n", backend.id()));
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
    let summary = if notes.is_empty() {
        format!("{} 条结果（{}）", hits.len(), backend.label())
    } else {
        // 降级了就在工具行上标出来，不点开也看得见
        format!("{} 条结果（{}，降级过）", hits.len(), backend.label())
    };
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
        let out = render("rust language", SearchBackend::DuckDuckGo, &hits, &[]);
        assert!(out.ok);
        assert!(out.content.contains("1. Rust"));
        assert!(out.content.contains("   https://rust-lang.org"));
        assert!(out.content.contains("2. (untitled)"));
        assert!(out.content.contains("web_fetch"));
        assert_eq!(out.summary, "2 条结果（DuckDuckGo）");
    }

    #[test]
    fn empty_results_say_so() {
        let out = render("zzz", SearchBackend::Brave, &[], &[]);
        assert!(out.ok);
        assert!(out.content.contains("No results"));
    }

    #[test]
    fn fallback_is_written_into_the_result() {
        let hits = vec![Hit {
            title: "T".into(),
            url: "https://x.example".into(),
            snippet: String::new(),
        }];
        let notes = vec![
            "DeepSeek failed: DeepSeek rate limited (HTTP 429)；稍等一会儿再搜，或换别的后端"
                .to_string(),
        ];
        let out = render("q", SearchBackend::Brave, &hits, &notes);
        // 说明在正文最前面，模型和用户都看得见
        assert!(out.content.starts_with("(DeepSeek failed: DeepSeek rate limited"));
        assert!(out.content.contains("via brave"));
        assert_eq!(out.summary, "1 条结果（Brave，降级过）");
    }

    #[test]
    fn parses_deepseek_results() {
        // 官方文档里的样例（脱敏）：有 summary 就用 summary，两段都留着
        let body = serde_json::json!({
            "results": [
                {
                    "rerank_score": 2.4,
                    "title": "DeepSeek V4 model released",
                    "url": "https://example.com/deepseek-v4",
                    "snippet": "DeepSeek V4 is now available, with tool calling and long-context support.",
                    "summary": "DeepSeek has released the V4 model.",
                    "content": "",
                    "site_name": "Example News",
                    "date": "2026-06-01T00:00:00+00:00",
                    "authority": 3
                },
                // 字段缺省：没有 summary / snippet / title
                { "url": "https://example.com/bare" },
                // 没有 url 的跳过
                { "title": "no url", "snippet": "x" }
            ]
        });
        let hits = parse_deepseek(&body);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "DeepSeek V4 model released");
        assert_eq!(hits[0].url, "https://example.com/deepseek-v4");
        assert!(hits[0].snippet.starts_with("DeepSeek has released the V4 model."));
        assert!(hits[0].snippet.contains("tool calling"));
        assert_eq!(hits[1].title, "");
        assert_eq!(hits[1].snippet, "");
    }

    #[test]
    fn deepseek_results_are_capped() {
        let results: Vec<Value> = (0..12)
            .map(|i| serde_json::json!({ "title": format!("t{i}"), "url": format!("https://x/{i}") }))
            .collect();
        let hits = parse_deepseek(&serde_json::json!({ "results": results }));
        assert_eq!(hits.len(), SEARCH_MAX_RESULTS);
        // 没有 results 字段（比如空结果）：空列表，不报错
        assert!(parse_deepseek(&serde_json::json!({ "results": [] })).is_empty());
        assert!(parse_deepseek(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn long_snippets_are_clipped() {
        let long = "字".repeat(SNIPPET_CHARS + 50);
        let clipped = clip(&long);
        assert_eq!(clipped.chars().count(), SNIPPET_CHARS + 1);
        assert!(clipped.ends_with('…'));
        assert_eq!(clip("短"), "短");
    }

    #[test]
    fn translates_search_errors() {
        assert!(explain_search_error("deepseek", Some(401), "").contains("key"));
        assert!(explain_search_error("deepseek", Some(401), "").contains("设置页"));
        assert!(explain_search_error("deepseek", Some(402), "").contains("充值"));
        assert!(explain_search_error("deepseek", Some(429), "").contains("rate limited"));
        assert!(explain_search_error("brave", Some(429), "").contains("Brave"));
        assert!(
            explain_search_error("deepseek", Some(500), "").contains("temporarily unavailable")
        );
        assert!(explain_search_error("deepseek", None, "timeout").contains("timeout"));
    }

    #[test]
    fn duckduckgo_401_does_not_blame_a_key() {
        let text = explain_search_error("duckduckgo", Some(401), "");
        assert!(!text.contains("key"), "{text}");
        assert!(explain_search_error("brave", Some(401), "").contains("Brave 的 key"));
    }

    #[test]
    fn default_order_matches_old_behaviour() {
        // 默认：Brave → DDG，DeepSeek 不参与（不悄悄花钱，配没配 key 都一样）
        let cfg = SearchConfig::default();
        assert_eq!(cfg.order(), vec![SearchBackend::Brave, SearchBackend::DuckDuckGo]);
        // 打开「也允许 DeepSeek」但没选它先用：排在最后，不会优先花这笔钱
        let allowed = SearchConfig { allow_deepseek: true, ..SearchConfig::default() };
        assert_eq!(
            allowed.order(),
            vec![SearchBackend::Brave, SearchBackend::DuckDuckGo, SearchBackend::DeepSeek]
        );
        // 优先项选了 DeepSeek：开关没开也算用户要它，提到最前
        let prefer = SearchConfig { prefer: SearchBackend::DeepSeek, ..SearchConfig::default() };
        assert_eq!(
            prefer.order(),
            vec![SearchBackend::DeepSeek, SearchBackend::Brave, SearchBackend::DuckDuckGo]
        );
        // 只允许 DeepSeek（没 key 时 `run` 会按「没 key」跳过）
        let only = SearchConfig {
            prefer: SearchBackend::DeepSeek,
            allow_deepseek: true,
            allow_brave: false,
            allow_duckduckgo: false,
        };
        assert_eq!(only.order(), vec![SearchBackend::DeepSeek]);
        // 全关：空链（`search` 报「没有可用的搜索后端」）
        let none =
            SearchConfig { allow_brave: false, allow_duckduckgo: false, ..Default::default() };
        assert!(none.order().is_empty());
    }

    #[test]
    fn search_json_round_trips_without_keys() {
        let cfg = SearchConfig {
            prefer: SearchBackend::DeepSeek,
            allow_deepseek: true,
            allow_brave: true,
            allow_duckduckgo: false,
        };
        // 存的形状：只有选择（枚举名和 Rust 里一致，反序列化回来对得上），没有 key
        let value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(value["prefer"], serde_json::json!("deepseek"));
        assert_eq!(value["allow_duckduckgo"], serde_json::json!(false));
        assert_eq!(value.as_object().unwrap().len(), 4);
        let back: SearchConfig = serde_json::from_value(value).unwrap();
        assert_eq!(back, cfg);
        // 手写的 JSON 少几个字段也认（`serde(default)`），没写 allow_deepseek 就是没允许
        let partial: SearchConfig = serde_json::from_str("{\"prefer\":\"duckduckgo\"}").unwrap();
        assert_eq!(partial.prefer, SearchBackend::DuckDuckGo);
        assert!(partial.allow_brave);
        assert!(!partial.allow_deepseek);
    }
}
