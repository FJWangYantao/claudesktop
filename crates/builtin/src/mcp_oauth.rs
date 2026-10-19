//! MCP 远程服务的 OAuth 2.1 授权（MCP 2025-06-18「Authorization」一节）。
//!
//! 服务器回 401 且带了 `WWW-Authenticate` 时走这里：资源元数据（RFC 9728）→ 授权服务器元数据
//! （RFC 8414）→ 动态客户端注册（RFC 7591）→ PKCE（RFC 7636）授权码 → 换 token，
//! 之后再拿 refresh token 续。
//!
//! - 网络只用 `ureq`，监听回调用自己的 `TcpListener`（照 `gateway.rs` 的做法，不引 HTTP 框架）。
//! - 授权页在系统浏览器里开：用户可能切去别的程序，我们的窗口不一定看得见，所以回调页是一张白页。
//! - token 存 `<数据目录>/mcp-auth/<服务器地址的哈希>.json`，按**地址**存：改名字不丢授权。
//! - `mcp.json` 里的 `headers.Authorization` 优先：写了就完全不碰这一套。
//!
//! 设计见仓库根目录「设计说明-MCP-OAuth.md」。

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 从出现「登录」按钮起，最多等用户多久回浏览器授权
pub(crate) const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
/// access token 离过期还有这么久就先换
const REFRESH_MARGIN_MS: u64 = 60_000;

/// `mcp.json` 里一个服务的 `"oauth"` 配置（都可以不写）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OAuthConfig {
    /// 授权服务器不支持自动注册（DCR）时手填
    pub client_id: Option<String>,
    /// 固定回调端口（授权服务器要求重定向地址精确匹配时用）
    pub callback_port: Option<u16>,
    pub scope: Option<String>,
}

/// 一次授权拿到的东西，也是落盘的那份。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Token {
    /// 哪台 MCP 服务器（原样的 url），刷新时用
    pub server: String,
    pub issuer: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// 换 token 的地址（刷新时不用再发现一遍）
    pub token_url: String,
    /// RFC 8707 的 `resource`
    pub resource: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// 毫秒时间戳；None = 授权服务器没说有效期
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

impl Token {
    /// 已经过期（或马上过期）且没有 refresh token 可用。
    fn is_dead(&self, now: u64) -> bool {
        self.refresh_token.is_none()
            && self.expires_at_ms.is_some_and(|at| at <= now + REFRESH_MARGIN_MS)
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// token 放哪：`<数据目录>/mcp-auth/`。
fn store_dir() -> Option<PathBuf> {
    claudesktop_core::default_data_dir().ok().map(|d| d.join("mcp-auth"))
}

/// 一台服务器对应的文件名：地址的 sha256 前 16 位（地址里带 `/` `:`，不能直接当文件名）。
fn file_stem(server: &str) -> String {
    let digest = Sha256::digest(server.trim_end_matches('/').as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// 读回这台服务器存着的 token；没有 / 坏了 / 已经没法续了都是 None。
pub(crate) fn load(server: &str) -> Option<Token> {
    let path = store_dir()?.join(format!("{}.json", file_stem(server)));
    let token: Token = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    (!token.is_dead(now_ms())).then_some(token)
}

/// 忘掉这台服务器的 token（被服务器拒绝了 / 用户在设置页点了「清除登录状态」）。
pub(crate) fn forget(server: &str) {
    if let Some(dir) = store_dir() {
        let _ = std::fs::remove_file(dir.join(format!("{}.json", file_stem(server))));
    }
}

/// 这台服务器有没有存着的 token（文件在、能解析就算有）。
///
/// 界面据此决定给不给「清除登录状态」（见 `McpServerView::has_auth`）；只看存在与格式，
/// 不把令牌内容交给任何调用方 —— 隐私上「界面不显示、日志不打」这条（设计说明第七节）靠这个守着。
pub fn has_token(server: &str) -> bool {
    token_file(server).is_some_and(|path| has_token_file(&path))
}

/// 这台服务器的 token 文件路径（数据目录不可用时 None）。
fn token_file(server: &str) -> Option<PathBuf> {
    Some(store_dir()?.join(format!("{}.json", file_stem(server))))
}

/// 这个 token 文件在且能解析（坏的 / 半份的当没有）。
fn has_token_file(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Token>(&text).ok())
        .is_some()
}

/// 存 token（先写临时文件再改名，崩溃时不留半份）。
pub(crate) fn save(token: &Token) -> Result<(), String> {
    let dir = store_dir().ok_or("找不到数据目录")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("建不了 {}：{e}", dir.display()))?;
    let path = dir.join(format!("{}.json", file_stem(&token.server)));
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(token).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("写不了 {}：{e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("存不下 token：{e}"))
}

/// 运行时的授权对象：`Http` 每次发送前取一次 access token，401 时换一次。
pub(crate) struct Auth {
    token: std::sync::Mutex<Token>,
    agent: ureq::Agent,
}

impl Auth {
    pub(crate) fn new(token: Token) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self { token: std::sync::Mutex::new(token), agent: agent() })
    }

    /// 现在能用的 access token（已经过期的也给出去：让它去撞 401 再刷新，省一次判断）。
    pub(crate) fn access_token(&self) -> String {
        self.token.lock().unwrap().access_token.clone()
    }

    /// 用 refresh token 换一个新的；换到就立刻落盘。没有 refresh token 或服务器拒绝了就返回原因。
    pub(crate) fn refresh(&self) -> Result<(), String> {
        let mut token = self.token.lock().unwrap();
        let refresh = token.refresh_token.clone().ok_or("没有 refresh token")?;
        let mut form = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh),
            ("client_id", token.client_id.clone()),
            ("resource", token.resource.clone()),
        ];
        if let Some(secret) = &token.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        let fresh = exchange(&self.agent, &token.token_url, &form)?;
        apply(&mut token, &fresh, now_ms());
        save(&token)?;
        Ok(())
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .build()
        .new_agent()
}

/// 一次授权里攒下来的东西，等用户回来。
pub(crate) struct Pending {
    /// 在浏览器里打开的授权页
    pub url: String,
    state: String,
    verifier: String,
    redirect: String,
    seed: Token,
    listener: TcpListener,
}

impl Pending {
    /// 等浏览器回调，换到 token 就存下并返回。超时 / 用户拒绝 / 换不到都返回原因。
    pub(crate) fn wait(self) -> Result<Token, String> {
        let code = accept_code(&self.listener, &self.state, LOGIN_TIMEOUT)?;
        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code),
            ("redirect_uri", self.redirect.clone()),
            ("client_id", self.seed.client_id.clone()),
            ("code_verifier", self.verifier.clone()),
            ("resource", self.seed.resource.clone()),
        ];
        if let Some(secret) = &self.seed.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        let fresh = exchange(&agent(), &self.seed.token_url, &form)?;
        let mut token = self.seed;
        apply(&mut token, &fresh, now_ms());
        save(&token)?;
        Ok(token)
    }
}

/// 把一次 token 响应的内容并进 `token`（`scope` / `refresh_token` 没给就保留原来的）。
/// `fresh.expires_at_ms` 是「还有多少毫秒」，这里换算成绝对时间。
fn apply(token: &mut Token, fresh: &Token, now: u64) {
    token.access_token = fresh.access_token.clone();
    if fresh.refresh_token.is_some() {
        token.refresh_token = fresh.refresh_token.clone();
    }
    if fresh.scope.is_some() {
        token.scope = fresh.scope.clone();
    }
    token.expires_at_ms = fresh.expires_at_ms.map(|left| now + left);
}

// ---------------------------------------------------------------------------
// 发现与注册
// ---------------------------------------------------------------------------

/// 401 的 `WWW-Authenticate` 里取资源元数据地址（RFC 9728）：多条 challenge 都看，
/// 只认带 `resource_metadata="…"` 的那条 Bearer。
pub(crate) fn resource_metadata_url(challenge: &str) -> Option<String> {
    let lower = challenge.to_ascii_lowercase();
    let at = lower.find("resource_metadata")?;
    let rest = challenge[at + "resource_metadata".len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    // 带引号的读到下一个引号，不带的读到逗号 / 空白
    let (value, closed) = match rest.strip_prefix('"') {
        Some(quoted) => (quoted.split('"').next().unwrap_or_default(), true),
        None => (rest.split([',', ' ']).next().unwrap_or_default(), false),
    };
    let url = value.trim();
    if !closed && url.is_empty() {
        return None;
    }
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}

/// 授权服务器元数据（RFC 8414）的几个可能地址：issuer 带路径时 `/.well-known/…` 插在中间；
/// 不认再退 OIDC 的 `openid-configuration`。
pub(crate) fn metadata_urls(issuer: &str) -> Vec<String> {
    let issuer = issuer.trim_end_matches('/');
    let Some(split) = issuer.find("://").map(|i| i + 3) else {
        return Vec::new();
    };
    let origin_end = issuer[split..].find('/').map_or(issuer.len(), |i| split + i);
    let (origin, path) = issuer.split_at(origin_end);
    let mut urls = Vec::new();
    for name in ["oauth-authorization-server", "openid-configuration"] {
        urls.push(format!("{origin}/.well-known/{name}{path}"));
        if !path.is_empty() {
            urls.push(format!("{origin}{path}/.well-known/{name}"));
        }
    }
    urls.dedup();
    urls
}

/// PKCE 的 S256 challenge：verifier 的 sha256 做 base64url（不带 padding）。
pub(crate) fn s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// 随机的 `state` / PKCE verifier（43～128 个 url-safe 字符）。
pub(crate) fn random_token(len: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut bytes = vec![0u8; len];
    // 拿不到系统随机源时退回时间戳，退路只为「能用」不为「够强」
    if getrandom::fill(&mut bytes).is_err() {
        let seed = now_ms() ^ (std::process::id() as u64) << 32;
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (seed.rotate_left(i as u32 * 7) % ALPHABET.len() as u64) as u8;
        }
    }
    bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

/// 拼授权页地址。`resource` 必须带（RFC 8707），否则拿到的 token 不认这台服务器。
pub(crate) fn authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect: &str,
    resource: &str,
    scope: Option<&str>,
    state: &str,
    challenge: &str,
) -> String {
    let mut params = vec![
        ("response_type", "code".to_string()),
        ("client_id", client_id.to_string()),
        ("redirect_uri", redirect.to_string()),
        ("state", state.to_string()),
        ("code_challenge", challenge.to_string()),
        ("code_challenge_method", "S256".to_string()),
        ("resource", resource.to_string()),
    ];
    if let Some(scope) = scope.filter(|s| !s.is_empty()) {
        params.push(("scope", scope.to_string()));
    }
    let sep = if authorization_endpoint.contains('?') { '&' } else { '?' };
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{}={}", pct(k), pct(v))).collect();
    format!("{authorization_endpoint}{sep}{}", query.join("&"))
}

/// 查询参数用的百分号编码（只留 unreserved，其余按 UTF-8 字节转）。
fn pct(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 授权服务器元数据里用得着的几项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AsMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
}

impl AsMetadata {
    fn from_json(v: &Value) -> Result<Self, String> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
        Ok(Self {
            issuer: text("issuer").unwrap_or_default().to_string(),
            authorization_endpoint: text("authorization_endpoint")
                .ok_or("授权服务器元数据里没有 authorization_endpoint")?
                .to_string(),
            token_endpoint: text("token_endpoint").ok_or("没有 token_endpoint")?.to_string(),
            registration_endpoint: text("registration_endpoint").map(str::to_string),
        })
    }
}

fn get_json(agent: &ureq::Agent, url: &str) -> Result<Value, String> {
    let response =
        agent.get(url).header("Accept", "application/json").call().map_err(|e| format!("{e}"))?;
    let status = response.status().as_u16();
    let body = response.into_body().read_to_string().unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}：{}", crate::provider::error_message(&body)));
    }
    serde_json::from_str(&body).map_err(|e| format!("不是 JSON：{e}"))
}

fn post_json(agent: &ureq::Agent, url: &str, body: &Value) -> Result<Value, String> {
    let response = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(body.to_string())
        .map_err(|e| format!("{e}"))?;
    let status = response.status().as_u16();
    let text = response.into_body().read_to_string().unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}：{}", crate::provider::error_message(&text)));
    }
    serde_json::from_str(&text).map_err(|e| format!("不是 JSON：{e}"))
}

/// 一步走完：资源元数据 → 授权服务器元数据 → 客户端注册 → 起回调监听 → 拼授权页。
pub(crate) fn prepare(
    server_url: &str,
    challenge: &str,
    cfg: &OAuthConfig,
) -> Result<Pending, String> {
    let agent = agent();
    let resource = server_url.to_string();
    // 1. 资源元数据：从 401 的挑战里拿地址，没给就按规范猜
    let prm_url = match resource_metadata_url(challenge) {
        Some(url) => url,
        None => guess_prm_url(server_url).ok_or("服务器没说授权服务器在哪，只能手填 headers")?,
    };
    let prm =
        get_json(&agent, &prm_url).map_err(|e| format!("读不了资源元数据（{prm_url}）：{e}"))?;
    let issuer = prm
        .get("authorization_servers")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .ok_or("资源元数据里没有 authorization_servers")?
        .to_string();

    // 2. 授权服务器元数据：几个候选地址依次试
    let mut last = String::new();
    let mut meta = None;
    for url in metadata_urls(&issuer) {
        match get_json(&agent, &url).and_then(|v| AsMetadata::from_json(&v)) {
            Ok(m) => {
                meta = Some(m);
                break;
            }
            Err(e) => last = format!("{url}：{e}"),
        }
    }
    let meta = meta.ok_or_else(|| format!("读不了授权服务器元数据（{last}）"))?;

    // 3. 回调监听先起：注册要用它的地址
    let port = cfg.callback_port.unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("绑不了本地回调端口 {port}：{e}"))?;
    let local = listener.local_addr().map_err(|e| e.to_string())?;
    let redirect = format!("http://127.0.0.1:{}/callback", local.port());

    // 4. 客户端：配置里手填的优先，否走动态注册
    let (client_id, client_secret) = match &cfg.client_id {
        Some(id) => (id.clone(), None),
        None => register(&agent, &meta, &redirect)?,
    };

    // 5. PKCE + state + 授权页
    let verifier = random_token(64);
    let state = random_token(32);
    let scope = cfg.scope.clone();
    let url = authorize_url(
        &meta.authorization_endpoint,
        &client_id,
        &redirect,
        &resource,
        scope.as_deref(),
        &state,
        &s256(&verifier),
    );
    Ok(Pending {
        url,
        state,
        verifier,
        redirect,
        seed: Token {
            server: resource.clone(),
            issuer: meta.issuer.clone(),
            client_id,
            client_secret,
            scope,
            token_url: meta.token_endpoint,
            resource,
            access_token: String::new(),
            refresh_token: None,
            expires_at_ms: None,
        },
        listener,
    })
}

/// 挑战里没给地址时的退路：`<origin>/.well-known/oauth-protected-resource`（规范里 resource 是服务器地址）。
fn guess_prm_url(server: &str) -> Option<String> {
    let split = server.find("://")? + 3;
    let origin_end = server[split..].find('/').map_or(server.len(), |i| split + i);
    Some(format!("{}/.well-known/oauth-protected-resource", &server[..origin_end]))
}

/// 动态客户端注册（RFC 7591）。不支持的服务器返回 `registration_endpoint` 为空的名，由调用方给提示。
fn register(
    agent: &ureq::Agent,
    meta: &AsMetadata,
    redirect: &str,
) -> Result<(String, Option<String>), String> {
    let url = meta.registration_endpoint.as_deref().ok_or(
        "授权服务器不支持自动注册（DCR），在 mcp.json 里给它填 \"oauth\": { \"client_id\": … }",
    )?;
    let body = serde_json::json!({
        "client_name": "claudesktop",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let reply = post_json(agent, url, &body).map_err(|e| format!("客户端注册失败：{e}"))?;
    let id = reply
        .get("client_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("客户端注册没有返回 client_id")?
        .to_string();
    let secret = reply.get("client_secret").and_then(Value::as_str).map(str::to_string);
    Ok((id, secret))
}

/// 用授权码 / refresh token 换 token（`application/x-www-form-urlencoded`）。
fn exchange(
    agent: &ureq::Agent,
    token_url: &str,
    form: &[(&str, String)],
) -> Result<Token, String> {
    let response = agent
        .post(token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send_form(form.iter().map(|(k, v)| (*k, v.as_str())))
        .map_err(|e| format!("连不上 {token_url}：{e}"))?;
    let status = response.status().as_u16();
    let body = response.into_body().read_to_string().unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!(
            "换 token 失败：HTTP {status}：{}",
            crate::provider::error_message(&body)
        ));
    }
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("换 token 不是 JSON：{e}"))?;
    let access_token = v
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("换 token 没有返回 access_token")?
        .to_string();
    Ok(Token {
        access_token,
        refresh_token: v.get("refresh_token").and_then(Value::as_str).map(str::to_string),
        scope: v.get("scope").and_then(Value::as_str).map(str::to_string),
        expires_at_ms: v.get("expires_in").and_then(Value::as_u64).map(|s| s * 1000),
        ..Token::default()
    })
}

// ---------------------------------------------------------------------------
// 回调
// ---------------------------------------------------------------------------

/// 等一次回调：拿 `?code=`（`state` 要对得上），回一张白页，然后关连接。
fn accept_code(
    listener: &TcpListener,
    expect_state: &str,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                match serve_callback(stream, expect_state) {
                    // 浏览器顺手要 favicon 之类：不是回调，接着等
                    Ok(None) => {}
                    Ok(Some(code)) => return Ok(code),
                    Err(e) => return Err(e),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("等授权回调超时了，再点一次「登录」".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("回调监听出错：{e}")),
        }
    }
}

/// 读一个请求，是回调就回页面并交出 code；不是回调返回 None。
fn serve_callback(mut stream: TcpStream, expect_state: &str) -> Result<Option<String>, String> {
    let mut line = String::new();
    BufReader::new(stream.try_clone().map_err(|e| e.to_string())?)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let target = line.split_whitespace().nth(1).unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/callback" {
        let _ = respond(&mut stream, 404, "找不到页面");
        return Ok(None);
    }
    let params = parse_query(query);
    // 用户点了拒绝：授权服务器把 error 带回来
    if let Some(error) = params.get("error") {
        let detail =
            params.get("error_description").map_or(error.clone(), |d| format!("{error}：{d}"));
        let _ = respond(&mut stream, 200, &format!("没有授权：{detail}"));
        return Err(format!("你在浏览器里拒绝了授权（{detail}）"));
    }
    if params.get("state").map(String::as_str) != Some(expect_state) {
        let _ = respond(&mut stream, 400, "state 对不上，这次授权作废");
        return Err("回调的 state 对不上，这次授权作废".into());
    }
    let Some(code) = params.get("code").filter(|c| !c.is_empty()) else {
        let _ = respond(&mut stream, 400, "回调里没有 code");
        return Err("回调里没有 code".into());
    };
    let _ = respond(&mut stream, 200, "授权完成，可以关掉这个标签页了。");
    Ok(Some(code.clone()))
}

/// 回一张极简白页（应用窗口可能不在前台，所以只靠浏览器自己显示）。
fn respond(stream: &mut TcpStream, status: u16, message: &str) -> std::io::Result<()> {
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>claudesktop</title>\
         <body style=\"font:16px system-ui;color:#1F1E1D;background:#FAF9F5;\
         display:flex;align-items:center;justify-content:center;height:100vh;margin:0\">\
         <p>{}</p></body>",
        message.replace('&', "&amp;").replace('<', "&lt;")
    );
    write!(
        stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    )?;
    stream.flush()
}

/// `a=1&b=2` → 表（百分号解码，`+` 当空格）。
fn parse_query(query: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(unpct(k), unpct(v));
    }
    out
}

/// 百分号解码（UTF-8）。
fn unpct(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_resource_metadata_url() {
        let one = r#"Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource""#;
        assert_eq!(
            resource_metadata_url(one).as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        // 多条 challenge、大小写混着、不带引号
        let many = r#"Basic realm="x", Bearer realm="mcp", Resource_Metadata=https://a.b/prm"#;
        assert_eq!(resource_metadata_url(many).as_deref(), Some("https://a.b/prm"));
        // 不带 `Bearer` 前缀的裸挑战
        assert_eq!(
            resource_metadata_url(r#"resource_metadata="http://127.0.0.1:9/x""#).as_deref(),
            Some("http://127.0.0.1:9/x")
        );
        assert_eq!(resource_metadata_url("Bearer realm=\"mcp\""), None);
        assert_eq!(resource_metadata_url(""), None);
        // 不是 http(s) 的不认
        assert_eq!(resource_metadata_url(r#"Bearer resource_metadata="ftp://x/y""#), None);
    }

    #[test]
    fn builds_metadata_urls_for_issuers_with_and_without_a_path() {
        assert_eq!(
            metadata_urls("https://auth.linear.app"),
            [
                "https://auth.linear.app/.well-known/oauth-authorization-server".to_string(),
                "https://auth.linear.app/.well-known/openid-configuration".to_string(),
            ]
        );
        // 带路径的：插在中间，再退「在自己路径下」
        let path = metadata_urls("https://example.com/tenant1/");
        assert_eq!(path[0], "https://example.com/.well-known/oauth-authorization-server/tenant1");
        assert_eq!(path[1], "https://example.com/tenant1/.well-known/oauth-authorization-server");
        assert!(path.iter().any(|u| u.ends_with("openid-configuration/tenant1")));
        assert!(metadata_urls("not a url").is_empty());
    }

    #[test]
    fn pkce_matches_the_rfc_7636_vector() {
        // RFC 7636 附录 B
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(s256(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let random = random_token(64);
        assert_eq!(random.len(), 64);
        assert!(random.chars().all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)));
        assert_ne!(random_token(64), random);
    }

    #[test]
    fn authorize_url_carries_resource_and_pkce() {
        let url = authorize_url(
            "https://auth.example.com/authorize",
            "client 1",
            "http://127.0.0.1:54545/callback",
            "https://mcp.example.com/mcp",
            Some("read write"),
            "st ate",
            "challenge",
        );
        assert!(url.starts_with("https://auth.example.com/authorize?"));
        for want in [
            "response_type=code",
            "client_id=client%201",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A54545%2Fcallback",
            "code_challenge=challenge",
            "code_challenge_method=S256",
            "resource=https%3A%2F%2Fmcp.example.com%2Fmcp",
            "scope=read%20write",
            "state=st%20ate",
        ] {
            assert!(url.contains(want), "缺 {want}：{url}");
        }
        // 端点自己带查询串时用 & 接
        let with_query = authorize_url("https://a/b?x=1", "c", "r", "s", None, "t", "u");
        assert!(with_query.starts_with("https://a/b?x=1&response_type=code"), "{with_query}");
        assert!(!with_query.contains("scope="));
    }

    #[test]
    fn parses_the_callback_query() {
        let params = parse_query("code=ab%2Bc&state=x+y&empty=");
        assert_eq!(params.get("code").map(String::as_str), Some("ab+c"));
        assert_eq!(params.get("state").map(String::as_str), Some("x y"));
        assert_eq!(params.get("empty").map(String::as_str), Some(""));
        let params = parse_query("error=access_denied&error_description=no");
        assert_eq!(params.get("error").map(String::as_str), Some("access_denied"));
        // 坏掉的百分号原样留着
        assert_eq!(parse_query("a=%zz").get("a").map(String::as_str), Some("%zz"));
    }

    #[test]
    fn guesses_the_protected_resource_metadata_url() {
        assert_eq!(
            guess_prm_url("https://mcp.example.com/mcp").as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(guess_prm_url("nope"), None);
    }

    #[test]
    fn token_files_round_trip_by_server_address() {
        // 文件名的哈希：同一个地址稳定，不同地址不同
        assert_eq!(file_stem("https://a/mcp"), file_stem("https://a/mcp/"));
        assert_ne!(file_stem("https://a/mcp"), file_stem("https://b/mcp"));
        assert_eq!(file_stem("https://a/mcp").len(), 16);

        let mut token = Token {
            server: "https://a/mcp".into(),
            issuer: "https://auth.a".into(),
            client_id: "c".into(),
            token_url: "https://auth.a/token".into(),
            resource: "https://a/mcp".into(),
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            ..Token::default()
        };
        // 有效期按秒给，换算成绝对时间
        let fresh = Token { expires_at_ms: Some(30_000), ..Token::default() };
        apply(&mut token, &fresh, 1_000);
        assert_eq!(token.expires_at_ms, Some(31_000));
        // 没给 refresh token 时保留原来的
        assert_eq!(token.refresh_token.as_deref(), Some("rt"));

        let now = now_ms();
        // 没有 refresh token 且已过期 = 没法续
        let dead = Token { refresh_token: None, expires_at_ms: Some(now - 1), ..token.clone() };
        assert!(dead.is_dead(now));
        // 有 refresh token 就不算死
        assert!(!Token { refresh_token: Some("x".into()), ..dead.clone() }.is_dead(now));
        // 没说有效期的也不算死
        assert!(!Token { expires_at_ms: None, ..dead }.is_dead(now));
    }

    #[test]
    fn only_half_built_pendings_are_rejected_early() {
        // 授权服务器不支持 DCR、配置里也没给 client_id：报清楚
        let meta = AsMetadata {
            issuer: "https://a".into(),
            authorization_endpoint: "https://a/authorize".into(),
            token_endpoint: "https://a/token".into(),
            registration_endpoint: None,
        };
        let err = register(&agent(), &meta, "http://127.0.0.1:1/callback").unwrap_err();
        assert!(err.contains("client_id"), "{err}");
    }

    #[test]
    fn has_token_only_counts_a_parsable_file() {
        // 没存过的地址没有令牌（只读，不碰数据目录里已有的东西）
        assert!(!has_token("https://never-saved.example.invalid/mcp"));

        // 坏掉的 / 半份的当没有；解析得出来的才算有。用临时目录，别往数据目录写
        let dir = std::env::temp_dir().join(format!("claudesktop-mcp-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = "https://has-token-test.example.invalid/mcp";
        let path = dir.join(format!("{}.json", file_stem(server)));
        assert!(!has_token_file(&path));
        std::fs::write(&path, "{ 坏掉的 json").unwrap();
        assert!(!has_token_file(&path));
        let token = Token { server: server.into(), access_token: "at".into(), ..Token::default() };
        std::fs::write(&path, serde_json::to_string(&token).unwrap()).unwrap();
        assert!(has_token_file(&path));
        // 过期到没法续的 token 文件仍然算「存着登录状态」：清除按钮就该出现
        let stale =
            Token { refresh_token: None, expires_at_ms: Some(now_ms().saturating_sub(1)), ..token };
        std::fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();
        assert!(has_token_file(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
