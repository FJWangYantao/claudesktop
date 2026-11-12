//! magpie 本地网关 sidecar：下载、启动、探活（见「设计说明-本机agent.md」第十四节）。
//!
//! [magpie](https://github.com/yetone/magpie)（MIT）把本机各家订阅和 key 统一成一个
//! 本地网关，OpenAI / Anthropic / Gemini 三种协议直通。自研内核把它当一个普通
//! Anthropic 端点用：base URL `http://127.0.0.1:<port>/v1`、key [`GATEWAY_KEY`]，
//! 模型列表走它家 `/v1/models`（`data[].id`，和 OpenAI 同形，`list_models` 直接能读）。
//!
//! 边界：**订阅登录的读取全部发生在 magpie 里**，claudesktop 不碰任何令牌文件；
//! 条款风险（各家订阅经第三方程序使用）已由用户拍板接受，界面上仍常驻一句提示。
//! 我们只做三件事：
//!
//! 1. 探活 `GET /api/hello`——网关已在跑（用户自己开的 magpie）就复用，不重复起；
//! 2. 下载 GitHub release 的 CLI 二进制（按平台选产物），按 `SHA256SUMS` 校验后落盘；
//! 3. 无界面启动 `magpie serve`——退出由 `claudesktop_core::kill_children_on_exit`
//!    的 job object 连带结束，这里不持有句柄。
//!
//! 端点、key、`/api/hello` 的约定见 magpie 的 `docs/integrating.md`（官方支持
//! 第三方 app 接网关，key 写 `magpie-<应用名>` 按名字记账）。

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// 自研内核连网关用的 key：`magpie-<应用名>`，magpie 按它把用量记到我们名下。
pub const GATEWAY_KEY: &str = "magpie-claudesktop";
/// 网关默认端口；magpie 的 `settings.json` 里写了别的就按那份。
pub const DEFAULT_PORT: u16 = 3425;
const REPO: &str = "yetone/magpie";
const API_LATEST: &str = "https://api.github.com/repos/yetone/magpie/releases/latest";

/// 本平台的 CLI 产物名（release 列表里的那个，SHA256SUMS 里的也是它）。
pub fn asset_name() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "magpie-cli-windows-amd64.exe",
        ("windows", "aarch64") => "magpie-cli-windows-arm64.exe",
        ("macos", "x86_64") => "magpie-cli-darwin-amd64",
        ("macos", "aarch64") => "magpie-cli-darwin-arm64",
        ("linux", "x86_64") => "magpie-cli-linux-amd64",
        ("linux", "aarch64") => "magpie-cli-linux-arm64",
        _ => "magpie-cli-linux-amd64",
    }
}

/// magpie 的设置文件（`~/.config/magpie/settings.json`；Windows 也是这个相对位置）。
fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(home).join(".config/magpie/settings.json"))
}

/// 网关端口：magpie 自己的 `settings.json` 里写了 `port` 就按它，否则默认。
pub fn gateway_port() -> u16 {
    settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| port_in_text(&text))
        .unwrap_or(DEFAULT_PORT)
}

/// 从 settings.json 的内容里抠 `port`（数字或字符串都认；拿不到用默认）。
fn port_in_text(text: &str) -> Option<u16> {
    let value: Value = serde_json::from_str(text).ok()?;
    let port = value.get("port")?;
    let n = port.as_u64().or_else(|| port.as_str()?.parse().ok())?;
    (n > 0 && n < u32::MAX as u64).then_some(n as u16)
}

/// 探活：网关在跑且真是 magpie 时返回它的版本号。1 秒连不上就当没在跑。
pub fn probe(port: u16) -> Option<String> {
    let agent = probe_agent();
    let url = format!("http://127.0.0.1:{port}/api/hello");
    let response = agent.get(&url).call().ok()?;
    let text = response.into_body().read_to_string().ok()?;
    parse_hello(&text)
}

/// `/api/hello` 的应答 → 版本号；`name` 不是 `magpie` 的不算（端口被别家占了）。
fn parse_hello(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("name")?.as_str()? != "magpie" {
        return None;
    }
    let version = value.get("version")?.as_str()?.trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// 一次性的 ureq agent：探活要快失败，别拿项目里 10 秒的那套。
fn probe_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(1)))
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .new_agent()
}

/// 问 GitHub API 用的：走代理时 3 秒不够。
fn api_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent()
}

/// sidecar 落盘位置：数据目录 `magpie/<可执行名>`（和 sessions、mcp.json 同一层）。
pub fn installed_path(data_dir: &Path) -> PathBuf {
    let name = if cfg!(windows) { "magpie.exe" } else { "magpie" };
    data_dir.join("magpie").join(name)
}

/// 已下载的 magpie 的版本号：跑一次 `--version`（3 秒超时、不弹窗口）。
/// 取不到 = None（不当成没装）。
pub fn installed_version(exe: &Path) -> Option<String> {
    let output = run_quiet(exe, &["--version"], Duration::from_secs(3))?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    claudesktop_core::agents::parse_version(&text)
}

/// 一次要被杀掉前拦住的子进程：stdin 关掉（不问交互问题），不弹窗口。
fn run_quiet(exe: &Path, args: &[&str], timeout: Duration) -> Option<std::process::Output> {
    let mut command = Command::new(exe);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    child.wait_with_output().ok()
}

/// 一次 release 要下什么：版本号、产物地址、校验和。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    /// 产物下载地址
    pub url: String,
    /// 字节数（进度用）
    pub size: u64,
    /// SHA256SUMS 里对上的那条
    pub sha256: String,
}

/// 查最新 release。产物地址、大小、SHA256SUMS 都从**同一次** API 应答里拿：
/// magpie 一天能出好几版，分开走 `/latest/download/` 会在两次请求之间撞上换版，
/// 校验和就对不上了。
pub fn latest_release() -> anyhow::Result<Release> {
    let agent = api_agent();
    let api = get_text(&agent, API_LATEST)?;
    let (tag, url, size, sums_url) = parse_release_meta(&api, asset_name())
        .ok_or_else(|| anyhow::anyhow!("最新 release 里没有 {REPO} 的 {} 产物", asset_name()))?;
    let sums = get_text(&agent, &sums_url)?;
    let sha256 = sha_from_sums(&sums, asset_name())
        .ok_or_else(|| anyhow::anyhow!("SHA256SUMS 里没有 {}", asset_name()))?;
    Ok(Release { tag, url, size, sha256 })
}

/// release 的 API JSON → (版本号, 产物地址, 产物大小, SHA256SUMS 地址)。
fn parse_release_meta(api: &str, asset: &str) -> Option<(String, String, u64, String)> {
    let value: Value = serde_json::from_str(api).ok()?;
    let tag = value.get("tag_name")?.as_str()?.trim().to_string();
    let assets = value.get("assets")?.as_array()?;
    let entry = assets.iter().find(|a| a.get("name").and_then(Value::as_str) == Some(asset))?;
    let url = entry.get("browser_download_url")?.as_str()?.to_string();
    let size = entry.get("size")?.as_u64()?;
    let sums_url = assets
        .iter()
        .find(|a| a.get("name").and_then(Value::as_str) == Some("SHA256SUMS"))?
        .get("browser_download_url")?
        .as_str()?
        .to_string();
    Some((tag, url, size, sums_url))
}

/// SHA256SUMS 的内容里抠某个产物的校验和（`<哈希>␣␣<产物名>` 一行一个）。
fn sha_from_sums(sums: &str, asset: &str) -> Option<String> {
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(hash), Some(name)) = (parts.next(), parts.next())
            && name == asset
            && hash.len() == 64
            && hash.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// 下载到 `to`，校验后原子落盘：
/// 先写旁边的 `.download`，比完哈希再改名；`progress(已下, 总量)` 每约 1 MB 报一次。
pub fn download(
    release: &Release,
    to: &Path,
    progress: &mut dyn FnMut(u64, u64),
    should_stop: &dyn Fn() -> bool,
) -> anyhow::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = to.with_extension("download");
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    let response = agent
        .get(&release.url)
        .call()
        .map_err(|e| anyhow::anyhow!("下载 {} 失败: {e}", release.url))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        anyhow::bail!("下载 {url} 失败: HTTP {status}", url = release.url);
    }
    let total = release.size;
    let mut reader = response.into_body().into_reader();
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::create(&tmp)?;
    let mut buf = [0u8; 128 << 10];
    let mut done = 0u64;
    let mut last_report = 0u64;
    loop {
        if should_stop() {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            anyhow::bail!("已停止");
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        done += n as u64;
        // 进度按 1 MB 一报（75 MB 的下载别每 128 KB 就去刷一遍界面）
        if done.saturating_sub(last_report) >= (1 << 20) || done == total {
            last_report = done;
            progress(done, total);
        }
    }
    if total > 0 && done != total {
        drop(file);
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("下载中断：收到 {done} / {total} 字节");
    }
    let hash = format!("{:x}", hasher.finalize());
    if !hash.eq_ignore_ascii_case(&release.sha256) {
        drop(file);
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("校验不符：期望 {}，实得 {hash}", release.sha256);
    }
    file.sync_all().ok();
    drop(file);
    // 老版本先挪开，改名这步不跨文件系统
    let old = to.with_extension("old");
    let _ = std::fs::remove_file(&old);
    if to.exists() {
        std::fs::rename(to, &old)?;
    }
    match std::fs::rename(&tmp, to) {
        Ok(()) => {
            let _ = std::fs::remove_file(&old);
            Ok(())
        }
        Err(err) => {
            let _ = std::fs::rename(&old, to);
            Err(err.into())
        }
    }
}

/// 无界面启动网关（`magpie serve`）。进程不持有句柄：应用退出时由
/// `kill_children_on_exit` 的 job object 连带结束；用户自己开的 magpie 不走这里
/// （探活探到就直接复用）。
pub fn spawn(exe: &Path) -> std::io::Result<std::process::Child> {
    let mut command = Command::new(exe);
    command.arg("serve").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command.spawn()
}

fn get_text(agent: &ureq::Agent, url: &str) -> anyhow::Result<String> {
    let response = agent.get(url).call().map_err(|e| anyhow::anyhow!("请求 {url} 失败: {e}"))?;
    let status = response.status().as_u16();
    let text = response.into_body().read_to_string().unwrap_or_default();
    if !(200..300).contains(&status) {
        anyhow::bail!("HTTP {status}: {text}");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// magpie 官方文档里写明的应答形状
    #[test]
    fn parses_hello() {
        assert_eq!(
            parse_hello(r#"{"name":"magpie","version":"v0.1.3866"}"#).as_deref(),
            Some("v0.1.3866")
        );
        assert_eq!(parse_hello(r#"{"name":"magpie"}"#), None);
        // 端口被别家占了：name 对不上不算
        assert_eq!(parse_hello(r#"{"name":"other","version":"1"}"#), None);
        assert_eq!(parse_hello("not json"), None);
    }

    #[test]
    fn reads_port_from_settings() {
        assert_eq!(port_in_text(r#"{"port": 3426}"#), Some(3426));
        // 有的程序把端口写成字符串
        assert_eq!(port_in_text(r#"{"port": "3426"}"#), Some(3426));
        assert_eq!(port_in_text("{}"), None);
        assert_eq!(port_in_text(r#"{"port": "x"}"#), None);
        assert_eq!(port_in_text("bad"), None);
        assert_eq!(port_in_text(r#"{"port": 0}"#), None);
    }

    /// release 的产物表：版本号、本平台产物的地址和大小、SHA256SUMS 的地址
    #[test]
    fn parses_release_meta() {
        let api = r#"{
            "tag_name": "v0.1.3866",
            "assets": [
                {"name": "magpie-darwin-arm64.dmg", "size": 39065923,
                 "browser_download_url": "https://github.com/yetone/magpie/releases/download/v0.1.3866/magpie-darwin-arm64.dmg"},
                {"name": "magpie-cli-windows-amd64.exe", "size": 75657728,
                 "browser_download_url": "https://github.com/yetone/magpie/releases/download/v0.1.3866/magpie-cli-windows-amd64.exe"},
                {"name": "SHA256SUMS", "size": 1351,
                 "browser_download_url": "https://github.com/yetone/magpie/releases/download/v0.1.3866/SHA256SUMS"}
            ]
        }"#;
        assert_eq!(
            parse_release_meta(api, "magpie-cli-windows-amd64.exe"),
            Some((
                "v0.1.3866".into(),
                "https://github.com/yetone/magpie/releases/download/v0.1.3866/magpie-cli-windows-amd64.exe".into(),
                75657728,
                "https://github.com/yetone/magpie/releases/download/v0.1.3866/SHA256SUMS".into(),
            ))
        );
        assert_eq!(parse_release_meta(api, "magpie-cli-plan9"), None);
    }

    /// SHA256SUMS 一行 `<哈希>␣␣<产物名>`：抠对得上名字的那条
    #[test]
    fn finds_sha_in_sums() {
        let sums = "\
f3145fe6 0x6ec74cdd7022cb930bdad7f30b78f87c0d947126911cdf185f18  magpie-cli-darwin-arm64\n\
00bc9f5b5c4ae0d17374248e5c6e324770a3b5f00bd1bafaceaeb34433767b18  magpie-cli-windows-amd64.exe\n\
";
        assert_eq!(
            sha_from_sums(sums, "magpie-cli-windows-amd64.exe").as_deref(),
            Some("00bc9f5b5c4ae0d17374248e5c6e324770a3b5f00bd1bafaceaeb34433767b18")
        );
        // 没有对得上的
        assert_eq!(sha_from_sums(sums, "magpie-cli-linux-amd64"), None);
        // 哈希不是 64 位十六进制的行不认
        assert_eq!(sha_from_sums("short  magpie-cli-linux-amd64", "magpie-cli-linux-amd64"), None);
    }

    /// sidecar 落盘的位置和数据目录同级，名字带平台后缀
    #[test]
    fn installs_under_data_dir() {
        let path = installed_path(Path::new("/data"));
        assert!(path.starts_with(Path::new("/data/magpie")));
        assert!(asset_name().starts_with("magpie-cli-"));
        // 产物名对得上平台
        if cfg!(windows) {
            assert!(asset_name().ends_with(".exe"));
        }
    }

    /// 实机探活：对正在跑的本地网关 probe 一次（配了 HTTP(S)_PROXY 的环境也要能秒回，
    /// 本机环回地址不走代理）。`cargo test -p claudesktop-builtin magpie -- --ignored`
    #[test]
    #[ignore = "实机，要本地有 magpie 网关"]
    fn probes_local_gateway() {
        let port = gateway_port();
        eprintln!("gateway_port() = {port}");
        let version = probe(port);
        eprintln!("probe({port}) = {version:?}");
        assert!(version.is_some(), "本地网关没在跑？");
    }

    /// 实机联测（联网，下载约 75 MB）：查最新 release → 下载 → 校验 → 读版本号。
    /// `cargo test -p claudesktop-builtin magpie -- --ignored`
    #[test]
    #[ignore = "联网，下载约 75 MB"]
    fn fetches_real_release() {
        let release = latest_release().expect("查 GitHub release");
        assert!(release.tag.starts_with('v'), "{:?}", release.tag);
        assert!(release.size > 50_000_000, "{}", release.size);
        assert_eq!(release.sha256.len(), 64);
        let dir = std::env::temp_dir().join("claudesktop-magpie-test");
        let _ = std::fs::remove_dir_all(&dir);
        let to = installed_path(&dir);
        let mut last = 0u64;
        download(
            &release,
            &to,
            &mut |done, total| {
                assert!(done <= total);
                last = total;
            },
            &|| false,
        )
        .expect("下载并校验");
        assert_eq!(last, release.size);
        assert!(to.is_file());
        let version = installed_version(&to).expect("跑 --version");
        assert!(!version.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
